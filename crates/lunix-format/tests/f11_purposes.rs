//! Freeze item 11: the key-purpose table. This crate builds the derivation
//! messages; the test derives with the reference primitives and compares with
//! the KAT outputs.

mod common;

use lunix_format::purpose::{Purpose, PurposeId, KDF_LABEL};
use lunix_format::*;

fn family() -> FamilyId {
    FamilyId(common::kat16("family"))
}

/// Default profile: keyed BLAKE3.
fn derive(root: &[u8; 32], p: &Purpose) -> [u8; 32] {
    common::purpose_key(root, &p.message(&family()).expect("message"))
}

/// FIPS profile: HKDF-SHA-256-Expand(PRK = root, info = msg, L = 32), which
/// for one block is HMAC-SHA-256(root, msg ‖ 0x01).
fn derive_fips(root: &[u8; 32], p: &Purpose) -> [u8; 32] {
    let mut info = p.message(&family()).expect("message");
    info.push(1);
    common::hmac_sha256(root, &info)
}

#[test]
fn messages_and_kat_outputs() {
    let kmk = common::kat32("kmk");
    let fam = family();
    let cases: Vec<(Purpose, Option<(&str, &str)>)> = vec![
        (
            Purpose::DataAddressing,
            Some(("purpose_addressing", "purpose_fips_addressing")),
        ),
        (
            Purpose::Dek(DekGen(0)),
            Some(("purpose_dek0", "purpose_fips_dek0")),
        ),
        (
            Purpose::Dek(DekGen(1)),
            Some(("purpose_dek1", "purpose_fips_dek1")),
        ),
        (
            Purpose::Metadata(ObjectKind::Commit),
            Some(("purpose_meta_commit", "purpose_fips_meta_commit")),
        ),
        (
            Purpose::Metadata(ObjectKind::Lease),
            Some(("purpose_meta_lease", "purpose_fips_meta_lease")),
        ),
        (
            Purpose::DescriptorMac,
            Some(("purpose_volume_mac", "purpose_fips_volume_mac")),
        ),
        (
            Purpose::Fingerprint,
            Some(("purpose_pin", "purpose_fips_pin")),
        ),
        (Purpose::NodeAddressing, None),
        (Purpose::KeyringMac, None),
        (Purpose::IntegrityHmac, None),
        (Purpose::ExtentTag, None),
        (Purpose::IntegrityAddressing, None),
    ];
    let mut text = String::new();
    text.push_str(
        "# Key purposes (on-disk-formats.md 4.2). Root = KAT kmk (20..3f), family = KAT family (40..4f).\n\
         # message = \"lunix kdf v1\\0\" || family_id || purpose || args\n\
         # default = keyed BLAKE3(root, message); fips = HKDF-SHA-256-Expand(root, message, 32)\n",
    );
    for (p, kat) in &cases {
        let msg = p.message(&fam).expect("message");
        assert!(msg.starts_with(KDF_LABEL));
        assert_eq!(&msg[13..29], &fam.0);
        assert_eq!(msg[29], p.id().id());
        let d = derive(&kmk, p);
        let f = derive_fips(&kmk, p);
        let line = match kat {
            Some((dk, fk)) => {
                assert_eq!(d.to_vec(), common::kat(dk), "default {dk}");
                assert_eq!(f.to_vec(), common::kat(fk), "fips {fk}");
                format!(
                    "{:#04x} {:<24} msg={} default={} fips={}  [KAT {dk}, {fk}]\n",
                    p.id().id(),
                    p.id().name(),
                    common::to_hex(&msg),
                    common::to_hex(&d),
                    common::to_hex(&f),
                )
            }
            None => format!(
                "{:#04x} {:<24} msg={} default={} fips={}  [no KAT]\n",
                p.id().id(),
                p.id().name(),
                common::to_hex(&msg),
                common::to_hex(&d),
                common::to_hex(&f),
            ),
        };
        text.push_str(&line);
    }
    // The KAT also records the DEK message itself.
    let dek1 = Purpose::Dek(DekGen(1)).message(&fam).expect("message");
    assert_eq!(dek1, common::kat("purpose_msg_dek1"));
    text.push_str(&format!(
        "purpose_msg_dek1 = {}\n",
        common::to_hex(&common::kat("purpose_msg_dek1"))
    ));
    // The fingerprint is the first 16 bytes of purpose 0x05.
    assert_eq!(
        derive(&kmk, &Purpose::Fingerprint)[..16],
        common::kat("purpose_pin")[..16]
    );
    common::golden_text("11-purposes.txt", &text);
}

#[test]
fn volume_a_and_b_keys_of_the_worked_examples() {
    // Volume B: IK 60..7f, family 50..5f (on-disk-formats.md 23).
    let ik = common::volume_b_ik();
    let fam = FamilyId(common::VOLUME_B_FAMILY);
    let key = |p: Purpose| common::purpose_key(&ik, &p.message(&fam).expect("message"));
    assert_eq!(
        common::to_hex(&key(Purpose::DescriptorMac)),
        "d56ef6bb95bf87c7b14ec9ad25b9eae3132d9120d815b04d6f467207f705d972"
    );
    assert_eq!(
        common::to_hex(&key(Purpose::Fingerprint)[..16]),
        "92a4ec2689f62aab86bc8f278713183e"
    );
    assert_eq!(
        common::to_hex(&key(Purpose::IntegrityHmac)),
        "aa13505a19bd5e783cbc31582031cb918c1dfaa9aa6b94b27057acb06306f4df"
    );
    assert_eq!(
        common::to_hex(&key(Purpose::ExtentTag)),
        "8f5a8aa5bd1c61e6d9b3e275436f35de2673144fc8d12cb7aab4b39398b09322"
    );
    // Purpose 0x0a (amendment 2026-11-12), section 23's table of derived keys.
    assert_eq!(
        common::to_hex(&key(Purpose::IntegrityAddressing)),
        "3e596debe98cf9c82d88e17c63d875e3f16dc9334a2dbe870f4dd827679372b8"
    );
}

#[test]
fn id_table_and_modes() {
    for (i, p) in PurposeId::ALL.iter().enumerate() {
        assert_eq!(p.id() as usize, i + 1);
        assert_eq!(PurposeId::from_u8(p.id()).expect("id"), *p);
    }
    assert!(PurposeId::from_u8(0).is_err());
    assert_eq!(
        PurposeId::from_u8(0x0a).expect("0x0a"),
        PurposeId::IntegrityAddressing
    );
    assert!(PurposeId::from_u8(0x0b).is_err());
    assert!(PurposeId::from_u8(0xff).is_err());
    // Table 4.2, "used in mode".
    use CryptoMode::*;
    let used = |p: PurposeId| -> Vec<CryptoMode> {
        [Plain, Integrity, Encrypted]
            .into_iter()
            .filter(|m| p.allowed_in(*m))
            .collect()
    };
    assert_eq!(used(PurposeId::DataAddressing), vec![Encrypted]);
    assert_eq!(used(PurposeId::Dek), vec![Encrypted]);
    assert_eq!(used(PurposeId::Metadata), vec![Encrypted]);
    assert_eq!(used(PurposeId::DescriptorMac), vec![Integrity, Encrypted]);
    assert_eq!(used(PurposeId::Fingerprint), vec![Integrity, Encrypted]);
    assert_eq!(used(PurposeId::NodeAddressing), vec![Encrypted]);
    assert_eq!(used(PurposeId::KeyringMac), vec![Integrity, Encrypted]);
    assert_eq!(used(PurposeId::IntegrityHmac), vec![Integrity]);
    assert_eq!(used(PurposeId::ExtentTag), vec![Integrity]);
    assert_eq!(used(PurposeId::IntegrityAddressing), vec![Integrity]);
    assert_eq!(PurposeId::ALL.len(), 10);
    assert_eq!(PurposeId::IntegrityAddressing.id(), 0x0a);
    assert_eq!(
        PurposeId::IntegrityAddressing.name(),
        "integrity addressing key"
    );
    // Plain has no key at all.
    assert!(PurposeId::ALL.iter().all(|p| !p.allowed_in(Plain)));
    // The keyring is never sealed: it has no metadata key.
    assert!(Purpose::Metadata(ObjectKind::Keyring)
        .message(&FamilyId([0; 16]))
        .is_err());
}

#[test]
fn object_kind_table() {
    let ids: Vec<u8> = ObjectKind::ALL.iter().map(|k| k.id()).collect();
    assert_eq!(ids, vec![1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13]);
    for k in ObjectKind::ALL {
        assert_eq!(ObjectKind::from_u8(k.id()).expect("kind"), k);
    }
    for bad in [0u8, 4, 14, 200, 255] {
        assert!(ObjectKind::from_u8(bad).is_err(), "kind {bad}");
    }
    assert_eq!(ObjectKind::Commit.name(), "commit");
    assert_eq!(Form::from_u8(1).expect("form"), Form::Sealed);
    assert!(Form::from_u8(2).is_err(), "form 2 is reserved");
    assert_eq!(CryptoMode::Encrypted.extent_ovh(), 108);
    assert_eq!(CryptoMode::Integrity.extent_ovh(), 68);
    assert_eq!(CryptoMode::Plain.extent_ovh(), 68);
    assert_eq!(CryptoMode::Plain.envelope_form(), Form::Mac);
    assert_eq!(CryptoMode::Encrypted.envelope_form(), Form::Sealed);
    assert!(CryptoMode::from_u8(3).is_err());
    assert!(Profile::from_u8(2).is_err());
}
