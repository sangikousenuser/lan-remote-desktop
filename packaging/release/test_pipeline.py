import base64
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

SPEC = importlib.util.spec_from_file_location('release_pipeline', Path(__file__).with_name('pipeline.py'))
pipeline = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(pipeline)
SHA = 'a' * 40


class PipelineTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='lrd-release-test-')
        self.root = Path(self.temporary.name)

    def tearDown(self):
        self.temporary.cleanup()

    def candidate(self, signer=None, public=None):
        inputs = self.root / 'inputs'
        (inputs / 'windows').mkdir(parents=True)
        (inputs / 'macos').mkdir()
        with zipfile.ZipFile(inputs / 'windows/LAN-Remote-Desktop-Windows-Host.zip', 'w') as archive:
            for name in ('lrd-windows-host.exe', 'lrd-update-helper-1.2.3.exe', 'lrd-network-helper-1.2.3.exe', 'Start-Host.ps1',
                         'Test-Host.ps1', 'Configure-Firewall.ps1', 'manual-testing.md', 'README.txt'):
                archive.writestr('LAN-Remote-Desktop-Windows-Host/' + name, b'fixture only')
        with zipfile.ZipFile(inputs / 'macos/LAN-Remote-Desktop-macOS-client.zip', 'w') as archive:
            for name in ('Contents/MacOS/lrd-macos-client', 'Contents/Helpers/lrd-update-helper', 'Contents/Helpers/lrd-network-helper'):
                archive.writestr('LAN Remote Desktop.app/' + name, b'fixture only')
            archive.writestr('LAN Remote Desktop.app/Contents/Info.plist', plistlib.dumps({
                'LRDVersion': '1.2.3', 'LSMinimumSystemVersion': '10.13'}))
        (inputs / 'windows/LAN-Remote-Desktop-Windows-Setup.exe').write_bytes(b'setup fixture')
        if signer is None:
            signer = self.root / 'fixture-signer'
            signer.write_bytes(b'not executed')
        if public is None:
            public = self.root / 'fixture-public.hex'
            public.write_text('0' * 64)
        output = self.root / 'prepared'
        pipeline.prepare(inputs, output, '1.2.3', SHA, 123, signer, public)
        return output

    def evidence(self, output):
        return {'source_sha': SHA, 'candidate_run_id': 123,
                'inventory_sha256': pipeline.digest(output / 'inventory.json'),
                **{name: True for name in pipeline.CHECKS}}

    def test_inventory_and_acceptance_are_bound_to_exact_candidate(self):
        output = self.candidate()
        evidence = self.evidence(output)
        gates = {'schema': 1, 'implementation_complete': True}
        pipeline.check(output, evidence, gates, 'refs/tags/v1.2.3', SHA)
        for name, value in [('high_sierra_10_13', False), ('windows_11', 1),
                            ('inventory_sha256', 'b' * 64), ('candidate_run_id', 124)]:
            with self.assertRaises(ValueError):
                pipeline.check(output, {**evidence, name: value}, gates, 'refs/tags/v1.2.3', SHA)
        with self.assertRaises(ValueError):
            pipeline.check(output, evidence, {'schema': 1, 'implementation_complete': False}, 'refs/tags/v1.2.3', SHA)
        with self.assertRaises(ValueError):
            pipeline.check(output, evidence, gates, 'refs/heads/main', SHA)
        (output / 'roots' / pipeline.MAC / 'Contents/Info.plist').write_bytes(b'changed after review')
        with self.assertRaises(ValueError):
            pipeline.verify_inventory(output)

    def test_unsafe_archive_and_symlinks_never_escape(self):
        for name in ('../outside', '/outside', 'a/../outside', 'a\\outside', 'CON', 'a:stream', 'a.'): 
            with self.assertRaises(ValueError):
                pipeline.safe_path(name)
        archive = self.root / 'attack.zip'
        with zipfile.ZipFile(archive, 'w') as output:
            info = zipfile.ZipInfo('prefix/link')
            info.create_system = 3
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            output.writestr(info, '../outside')
        with self.assertRaises(ValueError):
            pipeline.extract(archive, 'prefix/', self.root / 'unpacked')
        self.assertFalse((self.root / 'outside').exists())

    def test_case_colliding_members_are_rejected(self):
        archive = self.root / 'duplicates.zip'
        with zipfile.ZipFile(archive, 'w') as output:
            output.writestr('prefix/File.txt', b'a')
            output.writestr('prefix/file.txt', b'b')
        with self.assertRaises(ValueError):
            pipeline.extract(archive, 'prefix/', self.root / 'unpacked')

    def test_provenance_rejects_fork_pr_wrong_commit_and_failed_workflow(self):
        valid = {'path': '.github/workflows/release-candidate.yml', 'head_sha': SHA,
                 'status': 'completed', 'conclusion': 'success', 'event': 'push',
                 'head_repository': {'full_name': pipeline.REPOSITORY}}
        pipeline.validate_candidate_run(valid, SHA)
        for name, value in [('event', 'pull_request'), ('head_sha', 'b'*40),
                            ('conclusion', 'failure'), ('head_repository', {'full_name': 'other/repo'})]:
            with self.assertRaises(ValueError):
                pipeline.validate_candidate_run({**valid, name: value}, SHA)

    @unittest.skipUnless(os.environ.get('LRD_RELEASE_TOOL'), 'offline signer must be built explicitly')
    def test_real_cli_signs_verifies_rejects_tampering_and_wrong_public_key(self):
        tool = Path(os.environ['LRD_RELEASE_TOOL']).resolve(strict=True)
        key = self.root / 'key/signing.pk8'
        public = self.root / 'public.hex'
        subprocess.run([str(tool), 'keygen', str(key), str(public)], check=True, capture_output=True)
        output = self.candidate(tool, public)
        signed = self.root / 'signed'
        with patch.dict(os.environ, {'LRD_SIGNING_KEY_PKCS8_BASE64': base64.b64encode(key.read_bytes()).decode('ascii')}):
            pipeline.sign(output, signed)
        self.assertEqual(len(list(signed.iterdir())), 9)
        commands = []
        real_run = subprocess.run
        def run(command, **kwargs):
            if command[0] == 'gh':
                commands.append(command)
                return subprocess.CompletedProcess(command, 0)
            return real_run(command, **kwargs)
        release = {'draft': True, 'prerelease': False, 'assets': [
            {'name':p.name,'size':p.stat().st_size,'digest':'sha256:'+pipeline.digest(p),'state':'uploaded'}
            for p in signed.iterdir()]}
        release['assets'][0]['digest'] = 'sha256:'+'0'*64
        with patch.object(pipeline.subprocess, 'run', side_effect=run), patch.object(
                pipeline.subprocess, 'check_output', side_effect=[SHA+'\n', json.dumps(release).encode()]):
            with self.assertRaises(ValueError):
                pipeline.publish(signed,'1.2.3',SHA,tool,public)
        self.assertFalse(any(command[:3]==['gh','release','edit'] for command in commands))
        package = signed / f'package-{pipeline.MAC}.lrdpkg'
        package.write_bytes(package.read_bytes() + b'corruption')
        result = subprocess.run([str(tool), 'verify', str(signed / f'manifest-{pipeline.MAC}.json'),
                                 str(package), str(public)], capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        wrong = self.root / 'wrong.hex'
        wrong.write_text('0'*64)
        result = subprocess.run([str(tool), 'sign', str(key), str(output / f'recipe-{pipeline.MAC}.json'),
                                 str(output / 'roots' / pipeline.MAC), str(self.root / 'wrong-output'), str(wrong)], capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / 'wrong-output').exists())


if __name__ == '__main__':
    unittest.main()
