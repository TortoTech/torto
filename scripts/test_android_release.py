"""Release boundary checks; no credentials, network or Android SDK required."""
import copy
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


package = load('package-android-release')
publish = load('publish-android-release')


class AndroidRelease(unittest.TestCase):
    def metadata(self):
        return {'applicationId': package.PACKAGE, 'variantName': 'release', 'elements': [
            {'filters': [{'filterType': 'ABI', 'value': abi}], 'outputFile': 'app-{}.apk'.format(abi),
             'versionName': '0.9.0', 'versionCode': 9000} for abi in package.ABIS]}

    def test_metadata_requires_production_version_and_complete_split_set(self):
        package.apk_outputs(self.metadata(), '0.9.0', package.ABIS)
        for field, value in [('applicationId', package.PACKAGE + '.debug'), ('variantName', 'debug')]:
            data = self.metadata()
            data[field] = value
            with self.assertRaises(ValueError):
                package.apk_outputs(data, '0.9.0', package.ABIS)
        for field, value in [('versionName', '0.9.0-dev'), ('versionCode', 9001),
                             ('outputFile', '../escape.apk'), ('filters', []),
                             ('filters', [{'filterType': 'ABI', 'value': 'armeabi-v7a'}])]:
            data = self.metadata()
            data['elements'][0][field] = value
            with self.assertRaises(ValueError):
                package.apk_outputs(data, '0.9.0', package.ABIS)
        for elements in [self.metadata()['elements'][:1], [self.metadata()['elements'][0]] * 2]:
            data = self.metadata()
            data['elements'] = elements
            with self.assertRaises(ValueError):
                package.apk_outputs(data, '0.9.0', package.ABIS)

    def test_actual_apk_manifest_and_signer_must_match(self):
        badging = "package: name='org.tortotech.torto' versionCode='9000' versionName='0.9.0'\nnative-code: 'arm64-v8a'\n"
        package.verify_badging(badging, '0.9.0', 'arm64-v8a')
        for invalid in [badging + 'application-debuggable\n', badging.replace('9000', '9001'),
                        badging.replace('org.tortotech.torto', 'org.tortotech.torto.debug'),
                        badging.replace("'arm64-v8a'", "'arm64-v8a' 'x86_64'")]:
            with self.assertRaises(ValueError):
                package.verify_badging(invalid, '0.9.0', 'arm64-v8a')
        signer = 'Signer #1 certificate SHA-256 digest: ' + package.RELEASE_CERTIFICATE + '\n'
        package.verify_certificate(signer, package.RELEASE_CERTIFICATE)
        for invalid in ['', signer.replace(package.RELEASE_CERTIFICATE, 'a' * 64),
                        signer + signer, signer + 'Signer #1 certificate DN: CN=Android Debug\n']:
            with self.assertRaises(ValueError):
                package.verify_certificate(invalid, package.RELEASE_CERTIFICATE)

    def test_publication_is_resumable_but_never_overwrites_existing_payloads(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            assets = []
            for abi in package.ABIS:
                name = 'Torto-0.9.0-android-{}.apk'.format(abi)
                data = abi.encode()
                (directory / name).write_bytes(data)
                digest = hashlib.sha256(data).hexdigest()
                (directory / (name + '.sha256')).write_text('{}  {}\n'.format(digest, name))
            for file in directory.iterdir():
                assets.append({'name': file.name, 'size': file.stat().st_size,
                               'digest': 'sha256:' + hashlib.sha256(file.read_bytes()).hexdigest()})
            release = {'tag_name': 'v0.9.0', 'name': 'v0.9.0', 'draft': False, 'prerelease': False, 'assets': []}
            self.assertEqual(len(publish.publication_plan(release, 'v0.9.0', directory)), 4)
            release['assets'] = assets[:1]
            self.assertEqual(len(publish.publication_plan(release, 'v0.9.0', directory)), 3)
            release['assets'] = assets
            self.assertEqual(publish.publication_plan(release, 'v0.9.0', directory), [])
            invalid = copy.deepcopy(release)
            invalid['assets'][0]['digest'] = 'sha256:' + 'a' * 64
            with self.assertRaisesRegex(ValueError, 'differs'):
                publish.publication_plan(invalid, 'v0.9.0', directory)
            for field, value in [('draft', True), ('prerelease', True), ('name', 'Torto 0.9.0')]:
                invalid = dict(release, **{field: value})
                with self.assertRaises(ValueError):
                    publish.publication_plan(invalid, 'v0.9.0', directory)
            (directory / assets[0]['name']).write_bytes(b'corrupt')
            with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
                publish.publication_plan(release, 'v0.9.0', directory)


if __name__ == '__main__':
    unittest.main()
