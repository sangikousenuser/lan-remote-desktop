import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('windows_binary_gate',Path(__file__).with_name('verify_binary.py'))
gate=importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


def fixture(dll):
    data=bytearray(1024);data[:2]=b'MZ';struct.pack_into('<I',data,60,128)
    struct.pack_into('<4sHH',data,128,b'PE\0\0',0x8664,1)
    struct.pack_into('<H',data,148,240)
    optional=152
    struct.pack_into('<H',data,optional,0x20b)
    struct.pack_into('<I',data,optional+60,512)
    struct.pack_into('<I',data,optional+108,16)
    struct.pack_into('<II',data,optional+120,0x1000,40)
    struct.pack_into('<IIII',data,optional+240+8,512,0x1000,512,512)
    struct.pack_into('<I',data,512+12,0x1080)
    encoded=dll.encode('ascii')+b'\0';data[640:640+len(encoded)]=encoded
    return data


class BinaryTests(unittest.TestCase):
    def test_x64_system_import_and_external_runtime_rejection(self):
        self.assertEqual(gate.imports(fixture('KERNEL32.dll')),['kernel32.dll'])
        with tempfile.TemporaryDirectory() as temporary:
            path=Path(temporary)/'host.exe'
            for name in ['vcruntime140.dll','msvcp140.dll','libgcc_s_seh-1.dll']:
                path.write_bytes(fixture(name))
                with self.assertRaises(ValueError):gate.verify(path)

    def test_truncated_malformed_and_foreign_architecture_are_rejected(self):
        data=fixture('KERNEL32.dll')
        for cut in [0,2,63,127,151,390,600]:
            with self.assertRaises((ValueError,UnicodeDecodeError)):gate.imports(data[:cut])
        struct.pack_into('<H',data,132,0x14c)
        with self.assertRaises(ValueError):gate.imports(data)

    def test_import_pointer_outside_section_is_rejected(self):
        data=fixture('KERNEL32.dll');struct.pack_into('<I',data,524,0xffff0000)
        with self.assertRaises(ValueError):gate.imports(data)

    def test_delay_load_dependencies_are_not_missed(self):
        data=fixture('vcruntime140.dll')
        struct.pack_into('<II',data,152+120,0,0)
        struct.pack_into('<II',data,152+112+13*8,0x1000,64)
        data[512:576]=bytes(64)
        struct.pack_into('<II',data,512,1,0x1080)
        self.assertEqual(gate.imports(data),['vcruntime140.dll'])
        with tempfile.TemporaryDirectory() as temporary:
            path=Path(temporary)/'host.exe';path.write_bytes(data)
            with self.assertRaises(ValueError):gate.verify(path)


if __name__=='__main__':unittest.main()
