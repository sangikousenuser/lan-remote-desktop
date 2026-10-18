#!/usr/bin/env python3
# coding: utf-8
"""Package the Windows Ethernet Host, with an optional dual-mode NSIS installer."""
import argparse
import hashlib
import pathlib
import re
import shutil
import struct
import subprocess
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[2]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=pathlib.Path, required=True)
    parser.add_argument('--installer', action='store_true', help='also compile NSIS setup')
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    image = binary.read_bytes()
    if image[:2] != b'MZ' or len(image) < 64:
        parser.error('binary is not a Windows executable')
    offset = struct.unpack_from('<I', image, 60)[0]
    if image[offset:offset+4] != b'PE\0\0' or struct.unpack_from('<H', image, offset+4)[0] != 0x8664:
        parser.error('Windows x64 PE required')
    folder = ROOT / 'dist/windows-host'
    if folder.exists():
        shutil.rmtree(folder)
    folder.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, folder / 'lrd-windows-host.exe')
    helper = binary.with_name('lrd-update-helper.exe')
    if not helper.is_file():
        parser.error('build lrd-update-helper for the same Windows target first')
    shutil.copy2(helper, folder / helper.name)
    versions = re.findall(r'^version = "([0-9A-Za-z.+-]+)"$', (ROOT / 'Cargo.toml').read_text(), re.M)
    if len(versions) != 1:
        parser.error('one workspace version is required')
    version = versions[0]
    base_version = version.split('-')[0].split('+')[0]
    shutil.copy2(helper, folder / f'lrd-update-helper-{version}.exe')
    for name in ['Test-Host.ps1', 'Start-Host.ps1', 'Configure-Firewall.ps1']:
        source = ROOT / 'packaging/windows' / name
        (folder / name).write_text(source.read_text(encoding='utf-8-sig'), encoding='utf-8-sig')
    shutil.copy2(ROOT / 'docs/manual-testing.md', folder / 'manual-testing.md')
    (folder / 'README.txt').write_text(
        'LAN Remote Desktop 0.9.0-alpha.1 — Windows 11 x64 Host\n\n'
        'Start-Host.ps1 をPowerShellから実行し、物理Ethernet番号を選びます。\n'
        'Macで「探索」→「接続」。初回は両端のコード一致を確認して許可してください。\n'
        '通知領域アイコンを右クリックすると送信・受信フォルダー・切断・終了が使えます。\n'
        '受信先: Downloads\\LAN Remote Desktop\n\n'
        '手動起動: lrd-windows-host.exe --interfaces\n'
        '          lrd-windows-host.exe --serve --interface INDEX --auto\n'
        'Firewallに遮られる場合はmanual-testing.mdの限定規則を参照してください。\n'
        '通常のHostは管理者権限を必要としません。固定IP/DNS/経路は変更しません。\n'
        '正式版の全要件は未完了です。実装状況は同梱手順とソースdocsを参照してください。\n', encoding='utf-8-sig')
    archive = ROOT / 'dist/LAN-Remote-Desktop-Windows-Host.zip'
    with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as output:
        for source in sorted(folder.iterdir()):
            if source.is_file():
                output.write(source, 'LAN-Remote-Desktop-Windows-Host/' + source.name)
    with zipfile.ZipFile(archive) as check:
        assert check.testzip() is None
    artifacts = [archive]
    if args.installer:
        compiler = shutil.which('makensis')
        if not compiler:
            parser.error('NSIS makensis is required for --installer')
        # Native Windows makensis uses /D; the POSIX cross compiler uses -D.
        prefix = '/' if __import__('os').name == 'nt' else '-'
        subprocess.run([compiler, prefix+'NOCD', prefix+'V3', prefix+'DPAYLOAD='+str(folder), prefix+'DAPP_VERSION='+version, prefix+'DPRODUCT_VERSION='+base_version+'.0', str(ROOT / 'packaging/windows/installer.nsi')], cwd=ROOT / 'dist', check=True)
        artifacts.append(ROOT / 'dist/LAN-Remote-Desktop-Windows-Setup.exe')
    (ROOT / 'dist/windows-SHA256SUMS').write_text(''.join(hashlib.sha256(p.read_bytes()).hexdigest()+'  '+p.name+'\n' for p in artifacts))
    for artifact in artifacts:
        print(artifact)

if __name__ == '__main__':
    main()
