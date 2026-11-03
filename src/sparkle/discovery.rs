//! Read the signed, single-release appcast produced by prepare_sparkle_feed.py.
//! Discovery never constructs an updater, opens the profile, or downloads an enclosure.
use super::check::{ApprovedCheck, NOTES_LIMIT};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ring::signature::{UnparsedPublicKey, ED25519};
use std::{io::Read, time::Duration};

const LIMIT: usize = 512 * 1024;
const NS: &str = "http://www.andymatuschak.org/xml-namespaces/sparkle";
const INVALID: &str = "The signed PeerBrush release feed could not be verified.";
pub(super) type Offer = (String, String);

pub(super) fn fetch(config: &ApprovedCheck) -> Result<Option<Offer>, String> {
    let response = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(20))
        .build()
        .get(&config.feed_url)
        .call()
        .map_err(|_| "The PeerBrush release feed is unavailable.")?;
    if response.status() != 200 {
        return Err(INVALID.into());
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    read(&bytes, &config.public_ed_key, env!("CARGO_PKG_VERSION"))
}

fn child<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    ns: Option<&str>,
    name: &str,
) -> Result<roxmltree::Node<'a, 'input>, &'static str> {
    let mut found = node.children().filter(|n| {
        n.is_element() && n.tag_name().namespace() == ns && n.tag_name().name() == name
    });
    found
        .next()
        .filter(|_| found.next().is_none())
        .ok_or(INVALID)
}

fn read(bytes: &[u8], key: &str, installed: &str) -> Result<Option<Offer>, String> {
    if bytes.len() > LIMIT {
        return Err(INVALID.into());
    }
    let (xml, trailer) = std::str::from_utf8(bytes)
        .map_err(|_| INVALID)?
        .rsplit_once("<!-- sparkle-signatures:\nedSignature: ")
        .ok_or(INVALID)?;
    let (signature, length) = trailer
        .strip_suffix("\n-->\n")
        .ok_or(INVALID)?
        .split_once("\nlength: ")
        .ok_or(INVALID)?;
    if length != xml.len().to_string() {
        return Err(INVALID.into());
    }
    UnparsedPublicKey::new(&ED25519, STANDARD.decode(key).map_err(|_| INVALID)?)
        .verify(
            xml.as_bytes(),
            &STANDARD.decode(signature).map_err(|_| INVALID)?,
        )
        .map_err(|_| INVALID)?;
    let doc = roxmltree::Document::parse(xml).map_err(|_| INVALID)?;
    let root = doc.root_element();
    if !root.has_tag_name("rss") {
        return Err(INVALID.into());
    }
    let item = child(child(root, None, "channel")?, None, "item")?;
    let version = child(item, Some(NS), "version")?.text().ok_or(INVALID)?;
    let next = semver::Version::parse(version).map_err(|_| INVALID)?;
    if !next.pre.is_empty()
        || !next.build.is_empty()
        || child(item, Some(NS), "minimumSystemVersion")?.text() != Some("12.0.0")
        || child(item, Some(NS), "hardwareRequirements")?.text() != Some("arm64")
    {
        return Err(INVALID.into());
    }
    let description = child(item, None, "description")?;
    let notes = description.text().unwrap_or("");
    if description.attribute((NS, "format")) != Some("plain-text")
        || description.children().any(|n| n.is_element())
        || notes.len() > NOTES_LIMIT
    {
        return Err(INVALID.into());
    }
    Ok(
        (next > semver::Version::parse(installed).map_err(|_| INVALID)?)
            .then(|| (version.to_owned(), notes.to_owned())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    #[test]
    fn signed_offer_no_update_and_tampered_bytes() {
        let pair = Ed25519KeyPair::from_seed_unchecked(&[17; 32]).unwrap();
        let xml = format!(
            r#"<rss xmlns:s="{NS}"><channel><item><s:version>0.30.1</s:version><s:minimumSystemVersion>12.0.0</s:minimumSystemVersion><s:hardwareRequirements>arm64</s:hardwareRequirements><description s:format="plain-text">Brush &amp; pen</description></item></channel></rss>"#
        );
        let signed = format!(
            "{xml}<!-- sparkle-signatures:\nedSignature: {}\nlength: {}\n-->\n",
            STANDARD.encode(pair.sign(xml.as_bytes())),
            xml.len()
        );
        let key = STANDARD.encode(pair.public_key());
        assert_eq!(
            read(signed.as_bytes(), &key, "0.30.0").unwrap(),
            Some(("0.30.1".into(), "Brush & pen".into()))
        );
        assert_eq!(read(signed.as_bytes(), &key, "0.30.1").unwrap(), None);
        assert!(read(
            signed.replace("0.30.1", "0.30.2").as_bytes(),
            &key,
            "0.30.1"
        )
        .is_err());
        assert!(read(xml.as_bytes(), &key, "0.30.1").is_err());
    }
}
