"""Android TV pairing framing and exact device identity checks."""
import unittest
from unittest.mock import Mock

import pair_digi


class PairingTests(unittest.TestCase):
    def test_tcl_and_digi_identity_require_exact_model_and_vendor(self):
        pair_digi.validate_identity('Smart TV', 'TCL', 'tcl', 'Smart TV')
        pair_digi.validate_identity('DIGI R2A', 'ZTE', 'digi', None)
        for model, vendor, device, expected in [
                ('Smart TV', 'ZTE', 'tcl', 'Smart TV'),
                ('Other TV', 'TCL', 'tcl', 'Smart TV'),
                ('Smart TV', 'TCL', 'digi', None)]:
            with self.assertRaises(ValueError):
                pair_digi.validate_identity(model, vendor, device, expected)

    def test_frame_rejects_oversized_and_truncated_pairing_data(self):
        for data in [pair_digi.varint(8193), b'\x02\x01', b'\x80' * 5]:
            stream = Mock()
            pending = bytearray(data)
            def recv(size):
                result = bytes(pending[:size])
                del pending[:size]
                return result
            stream.recv.side_effect = recv
            with self.assertRaises(ValueError):
                pair_digi.frame(stream)


if __name__ == '__main__':
    unittest.main()
