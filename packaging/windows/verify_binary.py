#!/usr/bin/env python3
"""Bounded PE64 dependency gate: Windows system DLLs, no external VC runtime."""
import argparse
import json
import re
from pathlib import Path
import struct

SYSTEM = set('kernel32 ntdll advapi32 bcrypt bcryptprimitives d3d11 dxgi mf mfplat mfreadwrite '
             'ole32 oleaut32 propsys psapi shell32 shlwapi user32 userenv ws2_32 wtsapi32 '
             'iphlpapi secur32 crypt32 gdi32 dwmapi imm32 uxtheme winmm avrt ucrtbase '
             'comdlg32 comctl32 rpcrt4 version setupapi cfgmgr32 wintrust normaliz msvcrt dbghelp dbgcore'.split())


def imports(image):
    def read(offset, format):
        size = struct.calcsize(format)
        if offset < 0 or offset + size > len(image):
            raise ValueError('truncated PE structure')
        return struct.unpack_from(format, image, offset)
    if image[:2] != b'MZ':
        raise ValueError('DOS/PE image required')
    pe, = read(60, '<I')
    signature, machine, sections = read(pe, '<4sHH')
    optional_size, = read(pe+20, '<H')
    optional = pe+24
    if signature != b'PE\0\0' or machine != 0x8664 or read(optional, '<H')[0] != 0x20b:
        raise ValueError('Windows x64 PE32+ required')
    if not 1 <= sections <= 96 or optional_size < 128:
        raise ValueError('PE header/section limit')
    read(optional, f'<{optional_size}s')
    directories, = read(optional+108, '<I')
    headers_size, = read(optional+60, '<I')
    section_table = optional+optional_size
    ranges = []
    for index in range(sections):
        virtual_size, virtual, raw_size, raw = read(section_table+index*40+8, '<IIII')
        if raw+raw_size > len(image):
            raise ValueError('PE section outside file')
        ranges.append((virtual, virtual_size, raw, raw_size))
    def offset(rva, count):
        if 0 < rva < headers_size and rva+count <= min(headers_size,len(image)):
            return rva
        for virtual, _, raw, raw_size in ranges:
            if virtual <= rva and rva+count <= virtual+raw_size:
                return raw+rva-virtual
        raise ValueError('PE import RVA outside initialized data')
    def name(rva):
        start = offset(rva, 1)
        end = image.find(b'\0',start,min(start+261,len(image)))
        if end < 0:
            raise ValueError('unterminated import name')
        offset(rva,end-start+1)
        value = image[start:end].decode('ascii').lower()
        if not value or any(char in value for char in '/\\:'):
            raise ValueError('invalid DLL name')
        return value
    result = set()
    for directory, stride, name_at in ((1,20,12),(13,32,4)):
        if directories <= directory:
            continue
        if 112+(directory+1)*8 > optional_size:
            raise ValueError('PE directory outside optional header')
        rva, size = read(optional+112+directory*8, '<II')
        if rva == 0 and size == 0:
            continue
        if not rva or size < stride:
            raise ValueError('invalid import directory')
        for index in range(min(1024,size//stride)):
            at = offset(rva+index*stride,stride)
            descriptor = image[at:at+stride]
            if not any(descriptor):
                break
            if directory == 13 and read(at,'<I')[0] != 1:
                raise ValueError('unsupported delay import addressing')
            result.add(name(read(at+name_at,'<I')[0]))
        else:
            raise ValueError('unterminated/oversized import directory')
    return sorted(result)


def verify(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 512*1024*1024:
        raise ValueError('invalid executable')
    dependencies = imports(path.read_bytes())
    for dependency in dependencies:
        api_set=re.fullmatch(r'(?:api|ext)-ms-win-[a-z0-9-]+-l[0-9]+-[0-9]+-[0-9]+\.dll',dependency)
        if not dependency.endswith('.dll') or (dependency[:-4] not in SYSTEM and not api_set):
            raise ValueError(f'non-system DLL dependency: {dependency}; build MSVC with crt-static or bundle a reviewed dependency')
    print(json.dumps({'architecture':'x86_64','system_dependencies_only':True,'imports':dependencies}))
    return dependencies


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary',type=Path)
    verify(parser.parse_args().binary)
