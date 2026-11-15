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
    let build = child(item, Some(NS), "version")?.text().ok_or(INVALID)?;
    let mut short_versions = item
        .children()
        .filter(|n| n.has_tag_name((NS, "shortVersionString")));
    let version = match short_versions.next() {
        Some(short) => short.text().ok_or(INVALID)?,
        None => build,
    };
    if short_versions.next().is_some() || super::bundle_version(version) != build {
        return Err(INVALID.into());
    }
    let next = semver::Version::parse(version).map_err(|_| INVALID)?;
    let enclosure = child(item, None, "enclosure")?;
    let prefix = format!("https://github.com/Deftr0y/PeerBrush/releases/download/v{version}/PeerBrush-{version}-macOS-arm64-apple-signed-r");
    let revisions = enclosure
        .attribute("url")
        .and_then(|url| url.strip_prefix(&prefix))
        .and_then(|file| file.strip_suffix(".dmg"))
        .and_then(|name| name.split_once("-dmg-signed-r"))
        .ok_or(INVALID)?;
    let revision =
        |n: &str| n.len() <= 6 && n.parse::<u32>().is_ok_and(|v| v > 0 && v.to_string() == n);
    if !revision(revisions.0)
        || !revision(revisions.1)
        || enclosure.attribute("type") != Some("application/octet-stream")
        || !enclosure
            .attribute("length")
            .and_then(|n| n.parse::<u64>().ok())
            .is_some_and(|n| (1..=512 * 1024 * 1024).contains(&n))
        || !enclosure
            .attribute((NS, "edSignature"))
            .is_some_and(|s| STANDARD.decode(s).is_ok_and(|bytes| bytes.len() == 64))
    {
        return Err(INVALID.into());
    }
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
    let installed_version = semver::Version::parse(installed).map_err(|_| INVALID)?;
    let installed_build = semver::Version::parse(&super::bundle_version(installed))
        .map_err(|_| INVALID)?;
    let newer_build = semver::Version::parse(build).map_err(|_| INVALID)? > installed_build;
    Ok((newer_build && crate::updates::release_version_cmp(&next, &installed_version).is_gt())
        .then(|| (version.to_owned(), notes.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    #[test]
    fn signed_offer_no_update_and_tampered_bytes() {
        let pair = Ed25519KeyPair::from_seed_unchecked(&[17; 32]).unwrap();
        let xml = format!(
            r#"<rss xmlns:s="{NS}"><channel><item><s:version>0.30.1</s:version><s:minimumSystemVersion>12.0.0</s:minimumSystemVersion><s:hardwareRequirements>arm64</s:hardwareRequirements><description s:format="plain-text">Brush &amp; pen</description><enclosure url="https://github.com/Deftr0y/PeerBrush/releases/download/v0.30.1/PeerBrush-0.30.1-macOS-arm64-apple-signed-r1-dmg-signed-r2.dmg" length="300" type="application/octet-stream" s:edSignature="{}"/></item></channel></rss>"#,
            STANDARD.encode([7; 64])
        );
        let sign = |xml: &str| {
            format!(
                "{xml}<!-- sparkle-signatures:\nedSignature: {}\nlength: {}\n-->\n",
                STANDARD.encode(pair.sign(xml.as_bytes())),
                xml.len()
            )
        };
        let signed = sign(&xml);
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
        assert_eq!(read(signed.as_bytes(), &key, "0.4.0").unwrap(), None);
        let transition = xml.replace("0.30.1", "0.4.0").replace(
            "<s:version>0.4.0</s:version>",
            "<s:version>1.4.0</s:version><s:shortVersionString>0.4.0</s:shortVersionString>",
        );
        assert_eq!(
            read(sign(&transition).as_bytes(), &key, "0.30.1").unwrap(),
            Some(("0.4.0".into(), "Brush & pen".into()))
        );
        let mismatched_build = transition.replace("<s:version>1.4.0", "<s:version>1.4.1");
        assert!(read(sign(&mismatched_build).as_bytes(), &key, "0.30.1").is_err());
    }
}
