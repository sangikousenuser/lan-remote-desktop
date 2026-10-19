#!/usr/bin/env python3
"""Prepare immutable candidates; sign/publish only after explicit acceptance."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import stat
import subprocess
import tempfile
import zipfile

REPOSITORY = 'sangikousenuser/lan-remote-desktop'
WINDOWS = 'x86_64-pc-windows-msvc'
MAC = 'x86_64-apple-darwin'
LIMIT = 512 * 1024 * 1024
CHECKS = ('windows_11', 'high_sierra_10_13', 'update_rollback', 'install_modes',
          'lan_isolation', 'long_run', 'transport_comparison')


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def load(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 1024 * 1024:
        raise ValueError('invalid metadata file')
    return json.loads(path.read_text(encoding='utf-8'))


def write(path, value):
    with path.open('x', encoding='utf-8') as stream:
        json.dump(value, stream, sort_keys=True, indent=2)
        stream.write('\n')


def safe_path(name):
    path = PurePosixPath(name)
    if (not name or '\\' in name or path.is_absolute() or ':' in name
            or str(path) != name or any(part in ('', '.', '..') for part in path.parts)):
        raise ValueError('unsafe archive/member path')
    for part in path.parts:
        if (part.endswith(('.', ' ')) or len(part.encode('utf-8')) > 255
                or any(ord(char) < 32 or char in '<>"|?*' for char in part)
                or part.split('.')[0].upper() in {'CON', 'PRN', 'AUX', 'NUL', *[f'COM{i}' for i in range(1,10)], *[f'LPT{i}' for i in range(1,10)]}
                or part.lower() == '.lrd-update'):
            raise ValueError('cross-platform unsafe archive/member path')
    return path


def extract(archive, prefix, destination, allowed=None):
    total, members, seen = 0, [], set()
    with zipfile.ZipFile(archive) as source:
        if len(source.infolist()) > 1024:
            raise ValueError('archive entry limit')
        for entry in source.infolist():
            if not entry.filename.startswith(prefix) or entry.is_dir():
                continue
            name = entry.filename[len(prefix):]
            path = safe_path(name)
            if allowed is not None and name not in allowed:
                continue
            mode = entry.external_attr >> 16
            if stat.S_ISLNK(mode) or (stat.S_IFMT(mode) not in (0, stat.S_IFREG)):
                raise ValueError('archive special file rejected')
            folded = name.casefold()
            if folded in seen:
                raise ValueError('duplicate/case-colliding archive member')
            seen.add(folded)
            total += entry.file_size
            if total > LIMIT or len(members) >= 256:
                raise ValueError('archive expanded size/member limit')
            output = destination.joinpath(*path.parts)
            output.parent.mkdir(parents=True, exist_ok=True)
            with source.open(entry) as stream, output.open('xb') as target:
                data = stream.read(min(entry.file_size + 1, LIMIT + 1))
                if len(data) != entry.file_size:
                    raise ValueError('archive member length mismatch')
                target.write(data)
            executable = name.endswith('.exe') or name.startswith(('Contents/MacOS/', 'Contents/Helpers/'))
            output.chmod(0o755 if executable else 0o644)
            members.append({'path': name, 'executable': executable})
    if not members:
        raise ValueError('empty candidate')
    return sorted(members, key=lambda member: member['path'])


def prepare(inputs, output, version, sha, run_id, signer, public):
    if not re.fullmatch(r'\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?', version):
        raise ValueError('invalid candidate version')
    if not re.fullmatch(r'[a-f0-9]{40}', sha) or not re.fullmatch(r'[0-9]+', str(run_id)):
        raise ValueError('invalid candidate provenance')
    output.mkdir()
    assets = output / 'assets'
    assets.mkdir()
    archives = [(inputs / 'windows/LAN-Remote-Desktop-Windows-Host.zip', WINDOWS),
                (inputs / 'macos/LAN-Remote-Desktop-macOS-client.zip', MAC)]
    for archive, target in archives:
        shutil.copyfile(archive, assets / archive.name)
        root = output / 'roots' / target
        root.mkdir(parents=True)
        if target == WINDOWS:
            required = {'lrd-windows-host.exe', f'lrd-update-helper-{version}.exe',
                        'Start-Host.ps1', 'Test-Host.ps1', 'Configure-Firewall.ps1',
                        'manual-testing.md', 'README.txt'}
            members = extract(archive, 'LAN-Remote-Desktop-Windows-Host/', root, required)
            if {member['path'] for member in members} != required:
                raise ValueError('Windows payload incomplete')
            minimum = '10.0.22000'
        else:
            members = extract(archive, 'LAN Remote Desktop.app/', root)
            required = {'Contents/MacOS/lrd-macos-client', 'Contents/Helpers/lrd-update-helper', 'Contents/Info.plist'}
            if not required.issubset({member['path'] for member in members}):
                raise ValueError('Mac payload incomplete')
            info = plistlib.loads((root / 'Contents/Info.plist').read_bytes())
            if info.get('LRDVersion') != version or info.get('LSMinimumSystemVersion') != '10.13':
                raise ValueError('Mac version/minimum OS mismatch')
            minimum = '10.13.0'
        recipe = {'version': version, 'target': target, 'minimum_os': minimum, 'files': members,
                  'package_url': f'https://github.com/{REPOSITORY}/releases/download/v{version}/package-{target}.lrdpkg'}
        write(output / f'recipe-{target}.json', recipe)
    shutil.copyfile(inputs / 'windows/LAN-Remote-Desktop-Windows-Setup.exe', assets / 'LAN-Remote-Desktop-Windows-Setup.exe')
    shutil.copyfile(public, output / 'public-key.hex')
    (output / 'tools').mkdir()
    shutil.copyfile(signer, output / 'tools/signer')
    (output / 'tools/signer').chmod(0o755)
    files = {path.relative_to(output).as_posix(): digest(path) for path in sorted(output.rglob('*')) if path.is_file()}
    write(output / 'inventory.json', {'schema': 1, 'repository': REPOSITORY,
          'source_sha': sha, 'candidate_run_id': int(run_id), 'version': version, 'files': files})


def verify_inventory(root):
    inventory = load(root / 'inventory.json')
    if inventory['schema'] != 1 or inventory['repository'] != REPOSITORY:
        raise ValueError('candidate repository/schema mismatch')
    if not isinstance(inventory['files'], dict) or len(inventory['files']) > 1024:
        raise ValueError('inventory member limit')
    actual = {p.relative_to(root).as_posix() for p in root.rglob('*') if p.is_file()}
    if actual != set(inventory['files']) | {'inventory.json'}:
        raise ValueError('candidate file set changed')
    for name, expected in inventory['files'].items():
        path = root.joinpath(*safe_path(name).parts)
        parent = root
        for part in safe_path(name).parts:
            parent = parent / part
            if parent.is_symlink():
                raise ValueError('candidate symlink rejected')
        if not path.is_file() or digest(path) != expected:
            raise ValueError('candidate digest mismatch')
    return inventory


def check(root, evidence, gates, ref, sha):
    inventory = verify_inventory(root)
    version = inventory['version']
    if not re.fullmatch(r'\d+\.\d+\.\d+', version) or ref != f'refs/tags/v{version}':
        raise ValueError('Stable version tag is required')
    if gates != {'schema': 1, 'implementation_complete': True}:
        raise ValueError('implementation acceptance gate is still closed')
    expected = {'source_sha': sha, 'candidate_run_id': inventory['candidate_run_id'],
                'inventory_sha256': digest(root / 'inventory.json'), **{name: True for name in CHECKS}}
    if inventory['source_sha'] != sha or evidence != expected:
        raise ValueError('hardware evidence must match this exact candidate and all acceptance checks')
    if any(type(evidence.get(name)) is not bool for name in CHECKS):
        raise ValueError('acceptance checks must be explicit booleans')
    return inventory


def sign(root, destination):
    inventory = verify_inventory(root)
    if destination.exists():
        raise ValueError('signed output already exists')
    secret = os.environ.pop('LRD_SIGNING_KEY_PKCS8_BASE64', '')
    if not secret or len(secret) > 22000:
        raise ValueError('protected signing secret is missing/invalid')
    key = base64.b64decode(secret, validate=True)
    destination.mkdir()
    # No shell, build tools or application binaries run with the key present.
    with tempfile.TemporaryDirectory(prefix='lrd-sign-') as private:
        key_path = Path(private) / 'signing.pk8'
        descriptor = os.open(key_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, 'wb') as stream:
            stream.write(key)
        tool = root / 'tools/signer'
        tool.chmod(0o755)
        for target in (WINDOWS, MAC):
            subprocess.run([str(tool), 'sign', str(key_path), str(root / f'recipe-{target}.json'),
                            str(root / 'roots' / target), str(destination), str(root / 'public-key.hex')], check=True)
            subprocess.run([str(tool), 'verify', str(destination / f'manifest-{target}.json'),
                            str(destination / f'package-{target}.lrdpkg'), str(root / 'public-key.hex')], check=True)
    for path in (root / 'assets').iterdir():
        shutil.copyfile(path, destination / path.name)
    shutil.copyfile(root / 'inventory.json', destination / 'candidate-inventory.json')
    checksums = ''.join(f'{digest(p)}  {p.name}\n' for p in sorted(destination.iterdir()))
    (destination / 'SHA256SUMS').write_text(checksums, encoding='utf-8')
    print(f"Signed and verified {inventory['version']}; publication has not occurred.")


def validate_candidate_run(run, sha):
    if (run.get('path') != '.github/workflows/release-candidate.yml'
            or run.get('head_sha') != sha or run.get('status') != 'completed'
            or run.get('conclusion') != 'success' or run.get('event') not in ('push', 'workflow_dispatch')
            or run.get('head_repository', {}).get('full_name') != REPOSITORY):
        raise ValueError('candidate must be a successful first-party build of this exact commit')


def publish(root, version, sha, signer, public):
    if not re.fullmatch(r'\d+\.\d+\.\d+', version) or not re.fullmatch(r'[a-f0-9]{40}', sha):
        raise ValueError('invalid Stable publication')
    tag = 'v' + version
    names = {f'{kind}-{target}.{extension}' for target in (WINDOWS, MAC)
             for kind, extension in (('manifest', 'json'), ('package', 'lrdpkg'))}
    names |= {'LAN-Remote-Desktop-Windows-Host.zip', 'LAN-Remote-Desktop-Windows-Setup.exe',
              'LAN-Remote-Desktop-macOS-client.zip', 'candidate-inventory.json', 'SHA256SUMS'}
    if {path.name for path in root.iterdir()} != names or any(path.is_symlink() for path in root.iterdir()):
        raise ValueError('signed release asset set incomplete')
    checksums = dict((line.split('  ', 1)[1], line.split('  ', 1)[0]) for line in (root / 'SHA256SUMS').read_text().splitlines())
    if checksums != {name: digest(root / name) for name in names - {'SHA256SUMS'}}:
        raise ValueError('signed release bytes changed after signing')
    for target in (WINDOWS, MAC):
        subprocess.run([str(signer), 'verify', str(root / f'manifest-{target}.json'),
                        str(root / f'package-{target}.lrdpkg'), str(public)], check=True)
    def verify_tag():
        resolved = subprocess.check_output(['gh', 'api', f'repos/{REPOSITORY}/commits/{tag}', '--jq', '.sha'], text=True).strip()
        if resolved != sha:
            raise ValueError('release tag moved away from the accepted commit')
    verify_tag()
    # Existing releases/assets are never overwritten. Any failure leaves a draft.
    subprocess.run(['gh', 'release', 'create', tag, '--repo', REPOSITORY, '--draft', '--verify-tag',
                    '--target', sha, '--title', f'LAN Remote Desktop {version}',
                    '--notes', f'Accepted candidate {sha}. Signed updates and SHA256SUMS are attached.'], check=True)
    assets = sorted(path for path in root.iterdir() if path.is_file())
    subprocess.run(['gh', 'release', 'upload', tag, '--repo', REPOSITORY, *map(str, assets)], check=True)
    release = json.loads(subprocess.check_output(['gh', 'api', f'repos/{REPOSITORY}/releases/tags/{tag}']))
    expected = {p.name: (p.stat().st_size, 'sha256:' + digest(p)) for p in assets}
    actual = {p['name']: (p['size'], p.get('digest')) for p in release['assets'] if p['state'] == 'uploaded'}
    if not release['draft'] or release['prerelease'] or actual != expected:
        raise ValueError('uploaded draft asset digests do not match signed local bytes')
    verify_tag()
    subprocess.run(['gh', 'release', 'edit', tag, '--repo', REPOSITORY, '--draft=false', '--latest'], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=['prepare', 'check', 'sign', 'publish', 'candidate-run'])
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--version')
    parser.add_argument('--sha', default=os.environ.get('GITHUB_SHA', ''))
    parser.add_argument('--run-id', default=os.environ.get('GITHUB_RUN_ID', ''))
    parser.add_argument('--signer', type=Path)
    parser.add_argument('--public', type=Path, default=Path('packaging/update-public-key.hex'))
    args = parser.parse_args()
    if args.operation == 'prepare':
        prepare(args.root, args.output, args.version, args.sha, args.run_id, args.signer, args.public)
    elif args.operation == 'check':
        evidence = json.loads(os.environ.get('LRD_RELEASE_EVIDENCE', '{}'))
        check(args.root, evidence, load(Path('packaging/release/gates.json')), os.environ.get('GITHUB_REF', ''), args.sha)
    elif args.operation == 'sign':
        sign(args.root, args.output)
    elif args.operation == 'publish':
        publish(args.root, args.version, args.sha, args.signer, args.public)
    else:
        validate_candidate_run(load(args.root), args.sha)


if __name__ == '__main__':
    main()
