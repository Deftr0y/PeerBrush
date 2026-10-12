"""Cross-platform MSIX staging/validation regressions; fixtures are not applications."""
import base64
import hashlib
import io
import json
import pathlib
import struct
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile

import package_msix as msix
import test_package

ROOT = pathlib.Path(__file__).resolve().parents[1]


class Msix(unittest.TestCase):
    def setUp(self):
        self.fixture = test_package.Packages()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.root = self.fixture.root
        self.identity = msix.DEVELOPMENT_IDENTITY.copy()
        self.template = (ROOT/'packaging/msix/AppxManifest.xml.in').read_bytes()
        for name in ['AppxManifest.xml.in', *['Assets/'+n for n in msix.ASSETS]]:
            path = 'packaging/msix/'+name
            target = self.root/path
            target.parent.mkdir(parents=True,exist_ok=True)
            target.write_bytes((ROOT/path).read_bytes())
            self.fixture.policy['source'].append(path)
        self.fixture.save_policy()
        self.fixture.commit()
        self.archive = self.fixture.build('Windows')

    def stage(self):
        return msix.stage_payload(self.root,self.archive,self.root/'stage',self.identity)

    def rewrite_portable(self, change):
        with zipfile.ZipFile(self.archive) as archive:
            files={n:archive.read(n) for n in archive.namelist()}
        change(files)
        with zipfile.ZipFile(self.archive,'w') as archive:
            for n,data in files.items():archive.writestr(n,data)

    def test_manifest_retains_logo_alias_identity_and_scoped_permissions(self):
        report=self.stage()
        root=ET.parse(self.root/'stage/AppxManifest.xml').getroot()
        self.assertEqual(root.find('f:Identity',msix.NS).get('Version'),'1.2.0.0')
        self.assertEqual(root.find('f:Dependencies/f:TargetDeviceFamily',msix.NS).get('MinVersion'),'10.0.19041.0')
        self.assertEqual(report['commit'],msix.portable.git(self.root,'rev-parse','HEAD').decode().strip())
        self.assertTrue((self.root/'stage'/report['source']).is_file())
        self.assertTrue((self.root/'stage/LICENSE').is_file())
        for name,size in msix.ASSETS.items():
            data=(self.root/'stage/MsixAssets'/name).read_bytes()
            self.assertEqual(data,(ROOT/'packaging/msix/Assets'/name).read_bytes())
            self.assertEqual(struct.unpack('>II',data[16:24]),(size,size))
        text=(self.root/'stage/AppxManifest.xml').read_text()
        self.assertNotIn('AllowSilentDefaultTakeOver',text)
        self.assertNotIn('RegistryWriteVirtualization',text)
        self.assertNotIn('unvirtualizedResources',text)
        self.assertNotIn('FileSystemWriteVirtualization',text)
        self.assertNotIn('>disabled<',text)

    def test_staging_has_no_casefolded_directory_collisions_on_windows(self):
        self.stage()
        stage=self.root/'stage'
        paths=[p.relative_to(stage).as_posix() for p in stage.rglob('*') if p.is_file()]
        msix.validate_casefold_paths(paths)
        self.assertTrue((stage/'assets/peerbrush-logo.png').is_file())
        self.assertTrue((stage/'MsixAssets/StoreLogo.png').is_file())
        with self.assertRaisesRegex(ValueError,'directory collision'):
            msix.validate_casefold_paths(['assets/logo.png','Assets/StoreLogo.png'])
        with self.assertRaisesRegex(ValueError,'filename collision'):
            msix.validate_casefold_paths(['MsixAssets/logo.png','MSIXASSETS/LOGO.PNG'])
        with self.assertRaisesRegex(ValueError,'file collides'):
            msix.validate_casefold_paths(['assets','assets/logo.png'])

    def test_execution_alias_requires_explicit_native_entry_point(self):
        data = msix.manifest_bytes(self.template, self.identity, '0.2.0')
        root = ET.fromstring(data)
        alias = root.find('f:Applications/f:Application/f:Extensions/uap5:Extension', msix.NS)
        self.assertEqual(alias.get('Executable'), 'peerbrush.exe')
        self.assertEqual(alias.get('EntryPoint'), 'Windows.FullTrustApplication')
        for entry_point in [None, 'Incorrect.EntryPoint']:
            if entry_point is None:
                alias.attrib.pop('EntryPoint', None)
            else:
                alias.set('EntryPoint', entry_point)
            with self.assertRaisesRegex(ValueError, 'full-trust entry point'):
                msix.validate_manifest(ET.tostring(root), self.identity, '0.2.0')

    @unittest.skipUnless(msix.os.name == 'nt', 'Windows SDK validation requires Windows')
    def test_windows_sdk_accepts_synthetic_package_before_native_build(self):
        # Exercise the real SDK schema/semantic checks without compiling or
        # executing the synthetic PE, signing, installation, or trust changes.
        report = msix.build(self.root, self.archive, self.root/'sdk-package',
                            self.identity, development=True)
        self.assertFalse(report['signed'])
        self.assertFalse(report['installed_tested'])
        self.assertTrue((self.root/'sdk-package'/report['package']).is_file())

    def test_store_identity_is_required_and_local_identity_is_explicit(self):
        for identity in [{}, {'name':'x'}, {'name':'REPLACE_WITH_ID','publisher':'CN=Demo','publisher_display_name':'Demo'}]:
            with self.assertRaises(ValueError):msix.identity_values(identity)
        with self.assertRaisesRegex(ValueError,'development'):
            msix.identity_values(self.identity)
        self.assertEqual(msix.identity_values(self.identity,True),self.identity)
        for name in ['CON','com1.app','PeerBrush.','xn--peerbrush','Peer.xn--brush']:
            with self.assertRaises(ValueError):msix.identity_values(dict(self.identity,name=name),True)
        bad=dict(self.identity,publisher='CN=Test, OID.2.25.311729368913984317654407730594956997722=1')
        with self.assertRaisesRegex(ValueError,'unsigned-install'):msix.identity_values(bad,True)

    def test_reserved_store_identity_matches_supplied_family(self):
        identity = json.loads((ROOT/'packaging/msix/identity.store.json').read_text())
        product = json.loads((ROOT/'packaging/msix/product.store.json').read_text())
        msix.identity_values(identity)
        self.assertEqual(msix.validate_package_family(identity, product['package_family_name']), 'Deftroy.PeerBrush_wszhgmjq5tjta')
        data = msix.manifest_bytes(self.template, identity, '0.25.0')
        entry = ET.fromstring(data).find('f:Identity',msix.NS)
        self.assertEqual(entry.get('Name'),'Deftroy.PeerBrush')
        self.assertEqual(entry.get('Publisher'),'CN=438C1F79-CBE8-4F97-8C50-F9AC34C7D2AC')
        self.assertEqual(entry.get('Version'),'1.25.0.0')

    def test_family_check_rejects_wrong_identity_and_preserves_publisher_case(self):
        identity = json.loads((ROOT/'packaging/msix/identity.store.json').read_text())
        for bad in ['', 'Deftroy.PeerBrush_other', 'Deftroy.Other_wszhgmjq5tjta']:
            with self.assertRaises(ValueError): msix.validate_package_family(identity,bad)
        with self.assertRaisesRegex(ValueError,'do not match'):
            msix.validate_package_family(dict(identity,publisher=identity['publisher'].lower()),'Deftroy.PeerBrush_wszhgmjq5tjta')

    def test_portable_family_derivation_matches_microsoft_documented_reference(self):
        identity = {'name':'Microsoft.Windows.Photos','publisher':'CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US'}
        self.assertEqual(msix.derived_package_family(identity),'Microsoft.Windows.Photos_8wekyb3d8bbwe')

    def test_identity_xml_is_escaped_not_interpreted(self):
        identity=dict(self.identity,publisher='CN="A & B"',publisher_display_name='A < B')
        data=msix.manifest_bytes(self.template,identity,'0.2.0')
        self.assertEqual(ET.fromstring(data).find('f:Identity',msix.NS).get('Publisher'),'CN="A & B"')
        self.assertIn(b'A &lt; B',data)

    def test_version_mapping_is_monotonic_and_store_compatible(self):
        for cargo,expected in [('0.2.0','1.2.0.0'),('0.25.0','1.25.0.0'),('0.2.1','1.2.1.0'),('1.0.0','2.0.0.0')]:
            self.assertEqual(msix.package_version(cargo),expected)
        for cargo in ['0.2.0-dev','1.2','65535.0.0','1.65536.0']:
            with self.assertRaises(ValueError):msix.package_version(cargo)
        with self.assertRaises(ValueError):msix.package_version('0.2.0',1)
        self.assertEqual(msix.package_version('0.2.0',explicit='5.6.7.0',previous='5.6.6.0'),'5.6.7.0')
        for explicit in ['0.2.0.0','1.2.3.4','1.2.3','65536.0.0.0']:
            with self.assertRaises(ValueError):msix.package_version('0.2.0',explicit=explicit)
        for previous in ['1.2.0.0','1.3.0.0']:
            with self.assertRaises(ValueError):msix.package_version('0.2.0',previous=previous)

    def test_optional_image_types_are_opt_in_and_only_supported_types(self):
        data=msix.manifest_bytes(self.template,self.identity,'0.2.0',['.png','.jpg','.png'])
        root=ET.fromstring(data)
        types=[n.text for n in root.findall('.//uap:FileType',msix.NS)]
        self.assertEqual(types,['.psd','.psb','.jpg','.png'])
        with self.assertRaises(ValueError):msix.manifest_bytes(self.template,self.identity,'0.2.0',['.exe'])

    def test_stale_unrelated_files_do_not_enter_staging(self):
        (self.root/'private.txt').write_text('synthetic unrelated content')
        self.stage()
        self.assertFalse((self.root/'stage/private.txt').exists())
        with self.assertRaisesRegex(ValueError,'must be new'):self.stage()

    def test_staging_accepts_windows_crlf_notice_summary(self):
        def use_crlf(files):
            name=next(n for n in files if n.endswith('/THIRD_PARTY_NOTICES.md'))
            text=files[name].decode('utf-8').replace('\r\n','\n')
            files[name]=text.replace('\n','\r\n').encode('utf-8')
        self.rewrite_portable(use_crlf)
        self.stage()

    def test_staging_rejects_bare_carriage_return_notice_summary(self):
        def use_bare_cr(files):
            name=next(n for n in files if n.endswith('/THIRD_PARTY_NOTICES.md'))
            text=files[name].decode('utf-8').replace('\r\n','\n')
            files[name]=text.replace('\n','\r').encode('utf-8')
        self.rewrite_portable(use_bare_cr)
        with self.assertRaisesRegex(ValueError,'Missing third-party notice summary'):
            self.stage()

    def test_modified_executable_is_rejected(self):
        self.rewrite_portable(lambda files: files.__setitem__(next(n for n in files if n.endswith('/peerbrush.exe')),b'MZ tampered'))
        with self.assertRaisesRegex(ValueError,'checksum'):self.stage()

    def test_unlisted_private_material_is_rejected(self):
        self.rewrite_portable(lambda files: files.__setitem__(next(iter(files)).split('/')[0]+'/connection.json',b'synthetic private canary'))
        with self.assertRaisesRegex(ValueError,'Unexpected payload'):self.stage()

    def test_source_revision_and_tracked_edits_fail_closed(self):
        self.rewrite_portable(lambda files: files.__setitem__(next(n for n in files if n.endswith('/release.json')),json.dumps(dict(json.loads(files[next(n for n in files if n.endswith('/release.json'))]),commit='0'*40)).encode()))
        with self.assertRaisesRegex(ValueError,'exact commit'):self.stage()
        (self.root/'src/lib.rs').write_text('// changed')
        with self.assertRaisesRegex(ValueError,'Commit tracked'):self.stage()

    def test_required_source_notice_cannot_be_removed_from_self_declared_inventory(self):
        def remove_notice(files):
            key = next(name for name in files if name.endswith('/release.json'))
            report = json.loads(files[key])
            report['notice_files']['licenses/synthetic/NOTICE.txt'] = hashlib.sha256(b'Synthetic second notice').hexdigest()
            files[key.removesuffix('release.json') + 'licenses/synthetic/NOTICE.txt'] = b'Synthetic second notice'
            report['notice_files'].pop('licenses/ubuntu-sans/LICENCE.txt')
            files.pop(key.removesuffix('release.json') + 'licenses/ubuntu-sans/LICENCE.txt')
            files[key] = json.dumps(report).encode()
        self.rewrite_portable(remove_notice)
        with self.assertRaisesRegex(ValueError, 'source-derived notice'):
            self.stage()

    def test_store_build_requires_explicit_selected_input_hash(self):
        identity = {'name': 'Example.PeerBrush', 'publisher': 'CN=Example', 'publisher_display_name': 'Example'}
        with self.assertRaisesRegex(ValueError, 'selected portable SHA-256'):
            msix.build(self.root, self.archive, self.root/'output-msix', identity)
        with self.assertRaisesRegex(ValueError, 'differs from the selected input'):
            msix.build(self.root, self.archive, self.root/'output-msix', identity, expected_sha256='0'*64)

    def test_unknown_missing_packager_does_not_produce_msix(self):
        with self.assertRaises((FileNotFoundError,ValueError)):
            msix.find_makeappx(self.root/'no-sdk.exe')
        self.assertFalse(list(self.root.rglob('*.msix')))

    def test_unsafe_manifest_cannot_pass_semantic_check(self):
        data=msix.manifest_bytes(self.template,self.identity,'0.2.0')
        for old,new in [(b'peerbrush-mcp.exe',b'bad.exe'),(b'runFullTrust',b'broadFileSystemAccess'),(b'<rescap:Capability Name="runFullTrust" />',b'<rescap:Capability Name="runFullTrust" /><rescap:Capability Name="unvirtualizedResources" />'),(b'--open &quot;%1&quot;',b'--open %1')]:
            with self.assertRaises(ValueError):msix.validate_manifest(data.replace(old,new),self.identity,'0.2.0')

    def synthetic_msix(self, mutate=None):
        """Fixture for verifying ZIP/block-map logic, never a distributable MSIX."""
        self.stage()
        stage=self.root/'stage'
        files={p.relative_to(stage).as_posix():p.read_bytes() for p in stage.rglob('*') if p.is_file()}
        block_ns='http://schemas.microsoft.com/appx/2010/blockmap'
        root=ET.Element('BlockMap',xmlns=block_ns,HashMethod='http://www.w3.org/2001/04/xmlenc#sha256')
        for name,data in files.items():
            item=ET.SubElement(root,'File',Name=name.replace('/','\\'),Size=str(len(data)),LfhSize='0')
            for i in range(0,len(data),65536):
                ET.SubElement(item,'Block',Hash=base64.b64encode(hashlib.sha256(data[i:i+65536]).digest()).decode())
        files['AppxBlockMap.xml']=ET.tostring(root)
        files['[Content_Types].xml']=b'<Types />'
        if mutate:mutate(files)
        result=self.root/'synthetic.msix'
        with zipfile.ZipFile(result,'w') as archive:
            for name,data in files.items():archive.writestr(msix.opc_name(name),data)
        return result,stage

    def test_opc_names_encode_spaces_percent_and_unicode_without_aliases(self):
        self.assertEqual(msix.opc_name('Start PeerBrush.cmd'),'Start%20PeerBrush.cmd')
        self.assertEqual(msix.opc_name('Start%20PeerBrush.cmd'),'Start%2520PeerBrush.cmd')
        self.assertEqual(msix.opc_name('assets/作品.png'),'assets/%E4%BD%9C%E5%93%81.png')
        self.assertEqual(msix.opc_name('[Content_Types].xml'),'[Content_Types].xml')

    def test_final_archive_checks_every_payload_byte_and_block(self):
        path,stage=self.synthetic_msix()
        msix.verify_packed(path,stage)
        with zipfile.ZipFile(path,'a') as archive:archive.writestr('unlisted.txt','extra')
        with self.assertRaisesRegex(ValueError,'entries'):msix.verify_packed(path,stage)

    def test_tampered_block_map_is_rejected(self):
        def change(files):
            root=ET.fromstring(files['AppxBlockMap.xml'])
            next(root.iter('{http://schemas.microsoft.com/appx/2010/blockmap}Block')).set('Hash','invalid')
            files['AppxBlockMap.xml']=ET.tostring(root)
        path,stage=self.synthetic_msix(change)
        with self.assertRaisesRegex(ValueError,'block hash'):msix.verify_packed(path,stage)


if __name__=='__main__':unittest.main()
