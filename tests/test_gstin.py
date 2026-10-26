from structural import gstin


def test_check_char_roundtrip_and_detects_single_char_typo():
    g = gstin.with_check_char("24ZZACS0001A1Z")
    assert gstin.validate(g) == []
    typo = g[:5] + ("B" if g[5] != "B" else "C") + g[6:]
    assert "gstin_bad_checksum" in gstin.validate(typo)


def test_known_public_example():
    # Widely published sample GSTIN used in GST portal documentation examples.
    assert gstin.check_char("27AAPFU0939F1Z") == "V"


def test_pan_binding_and_format():
    g = gstin.with_check_char("24ZZACS0001A1Z")
    assert gstin.validate(g, master_pan="ZZACS0001A") == []
    assert "gstin_pan_mismatch" in gstin.validate(g, master_pan="ZZACS0009A")
    assert gstin.validate("not-a-gstin") == ["gstin_bad_format"]
