import unittest
from verify_binary import minimum_versions


class DeploymentTargetTests(unittest.TestCase):
    def test_legacy_minimum_not_dylib_version(self):
        loads = """
Load command 1
          cmd LC_LOAD_DYLIB
      version 2775.10.103
Load command 2
          cmd LC_VERSION_MIN_MACOSX
      version 10.13
          sdk 26.5
Load command 3
          cmd LC_SOURCE_VERSION
      version 9999.1
"""
        self.assertEqual(minimum_versions(loads), ["10.13"])

    def test_modern_minimum_not_tool_version(self):
        loads = """
Load command 1
          cmd LC_BUILD_VERSION
     platform 1
        minos 11.0
          sdk 27.0
        ntools 1
          tool 3
       version 1234.1
"""
        self.assertEqual(minimum_versions(loads), ["11.0"])

    def test_missing_minimum_is_not_a_pass(self):
        self.assertEqual(minimum_versions("cmd LC_LOAD_DYLIB\nversion 10.13"), [])


if __name__ == "__main__":
    unittest.main()
