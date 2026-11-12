# Attach matrix

Status: draft of M1.A0, 2026-11-12, part of the format sign-off
(`crates/lunix-format/FREEZE.md`).

This is the expected result of the attach check of ADR-11 F7 (the downgrade
defence) for every combination of its inputs. It was derived by hand from
the rules below. M2's `check_attach` test loads the tables of this file as
its expected results; M2 does not derive them.

Byte layouts referred to here are in [on-disk-formats.md](on-disk-formats.md):
the descriptor's `crypto_mode` and `kmk_fingerprint` (§11), the descriptor
MAC (§11), the local pin in `state/pins` (§20), the key purposes (§4.2).

## Inputs

| input | values | meaning |
|---|---|---|
| descriptor | `encrypted`, `unencrypted`, `integrity` | the `crypto_mode` of the `volume` descriptor. In Table A, `unencrypted` is mode `plain` (no keyring); mode `integrity` is in Tables B and C. |
| credential | `key`, `integrity`, `none` | what the attach request supplies. `key` = key material declaring an encrypted volume: a passphrase, key file or KMS reference that unwraps a slot of the family keyring. `integrity` = the credential of an integrity-only volume, declared as such by the request. `none` = no credential. A supplied credential is assumed to unwrap a slot; a credential that unwraps nothing (`WrongCredential`) and a keyring older than the pin (`KeyringRollback`) fail before the MAC check and are outside the matrix. |
| MAC | `valid`, `invalid` | whether the descriptor MAC verifies: under the descriptor MAC key derived from the unwrapped root (keyed modes), or as an unkeyed checksum (`plain`). |
| fingerprint | `right`, `wrong`, `absent` | the KMK fingerprint the orchestrator supplies (StorageClass, `LunixBackendSet` or the Secret). `right` = equal to the fingerprint derived from the unwrapped root. A `plain` volume has no fingerprint, so for it `right` and `wrong` both mean "a fingerprint was supplied". |
| pin | `right`, `wrong`, `absent` | the local trust-on-first-use pin in `state/pins` (interactive CLI). `right` = same mode and, for a keyed mode, same fingerprint. `wrong` = for a keyed descriptor, a different fingerprint or mode; for a `plain` descriptor, a pin that records a keyed mode. |

## Results

| result | meaning |
|---|---|
| `Ok` | attach proceeds; the pin is unchanged |
| `Ok+pin` | attach proceeds; when the caller keeps a pin store (the CLI), it records the pin `{mode, fingerprint}` (first use) |
| `Downgrade` | the request or the pin expects more protection than the descriptor has |
| `NeedCredential` | the descriptor is keyed and no credential was supplied |
| `ModeMismatch` | an integrity credential was supplied for an encrypted descriptor |
| `BadMac` | the descriptor MAC (or checksum) does not verify |
| `WrongKmk` | the volume's root key differs from the supplied fingerprint or the pin |

`NeedCredential` and `ModeMismatch` are added to M2's error list
(`Downgrade`, `WrongKmk`, `BadMac`, `KeyringRollback`).

## Check order (normative)

The first rule that applies decides the result.

1. **The request decides the mode** (ADR-11 F7).
   - Credential `key` and descriptor not `encrypted` → `Downgrade`.
   - Credential `integrity` and descriptor `plain` → `Downgrade`;
     descriptor `encrypted` → `ModeMismatch`.
   - Credential `none` and descriptor `encrypted` or `integrity` →
     `NeedCredential`.
2. **A `plain` descriptor with a keyed expectation:** a supplied
   fingerprint, or a pin that records a keyed mode → `Downgrade`.
3. Unwrap the slot and verify the keyring (failures are outside the
   matrix).
4. **Descriptor MAC** (checksum for `plain`) does not verify → `BadMac`. In
   a keyed mode, a stored `kmk_fingerprint` that differs from the derived
   one is also `BadMac` (the MAC covers the field, so this only catches a
   writer bug).
5. **Keyed mode, supplied fingerprint** differs from the derived one →
   `WrongKmk`.
6. **Keyed mode, pin** present and different (mode or fingerprint) →
   `WrongKmk`.
7. Otherwise `Ok` if a pin is present, `Ok+pin` if it is absent.

The orchestrator path keeps no pin store and passes pin `absent`; its
`Ok+pin` is an `Ok` with nothing recorded. Without a credential, a
fingerprint or a pin, a volume recreated as plaintext cannot be told from a
real one (ADR-11 F7): that is row 63.

## Table A: the plan's 72 combinations

Descriptor `encrypted` or `unencrypted` (= `plain`) × credential `key` or
`none` × MAC × fingerprint × pin.

| # | descriptor | credential | MAC | fingerprint | pin | result | rule |
|---|---|---|---|---|---|---|---|
| 1 | encrypted | key | valid | right | right | `Ok` | 7 |
| 2 | encrypted | key | valid | right | wrong | `WrongKmk` | 6 |
| 3 | encrypted | key | valid | right | absent | `Ok+pin` | 7 |
| 4 | encrypted | key | valid | wrong | right | `WrongKmk` | 5 |
| 5 | encrypted | key | valid | wrong | wrong | `WrongKmk` | 5 |
| 6 | encrypted | key | valid | wrong | absent | `WrongKmk` | 5 |
| 7 | encrypted | key | valid | absent | right | `Ok` | 7 |
| 8 | encrypted | key | valid | absent | wrong | `WrongKmk` | 6 |
| 9 | encrypted | key | valid | absent | absent | `Ok+pin` | 7 |
| 10 | encrypted | key | invalid | right | right | `BadMac` | 4 |
| 11 | encrypted | key | invalid | right | wrong | `BadMac` | 4 |
| 12 | encrypted | key | invalid | right | absent | `BadMac` | 4 |
| 13 | encrypted | key | invalid | wrong | right | `BadMac` | 4 |
| 14 | encrypted | key | invalid | wrong | wrong | `BadMac` | 4 |
| 15 | encrypted | key | invalid | wrong | absent | `BadMac` | 4 |
| 16 | encrypted | key | invalid | absent | right | `BadMac` | 4 |
| 17 | encrypted | key | invalid | absent | wrong | `BadMac` | 4 |
| 18 | encrypted | key | invalid | absent | absent | `BadMac` | 4 |
| 19 | encrypted | none | valid | right | right | `NeedCredential` | 1 |
| 20 | encrypted | none | valid | right | wrong | `NeedCredential` | 1 |
| 21 | encrypted | none | valid | right | absent | `NeedCredential` | 1 |
| 22 | encrypted | none | valid | wrong | right | `NeedCredential` | 1 |
| 23 | encrypted | none | valid | wrong | wrong | `NeedCredential` | 1 |
| 24 | encrypted | none | valid | wrong | absent | `NeedCredential` | 1 |
| 25 | encrypted | none | valid | absent | right | `NeedCredential` | 1 |
| 26 | encrypted | none | valid | absent | wrong | `NeedCredential` | 1 |
| 27 | encrypted | none | valid | absent | absent | `NeedCredential` | 1 |
| 28 | encrypted | none | invalid | right | right | `NeedCredential` | 1 |
| 29 | encrypted | none | invalid | right | wrong | `NeedCredential` | 1 |
| 30 | encrypted | none | invalid | right | absent | `NeedCredential` | 1 |
| 31 | encrypted | none | invalid | wrong | right | `NeedCredential` | 1 |
| 32 | encrypted | none | invalid | wrong | wrong | `NeedCredential` | 1 |
| 33 | encrypted | none | invalid | wrong | absent | `NeedCredential` | 1 |
| 34 | encrypted | none | invalid | absent | right | `NeedCredential` | 1 |
| 35 | encrypted | none | invalid | absent | wrong | `NeedCredential` | 1 |
| 36 | encrypted | none | invalid | absent | absent | `NeedCredential` | 1 |
| 37 | unencrypted | key | valid | right | right | `Downgrade` | 1 |
| 38 | unencrypted | key | valid | right | wrong | `Downgrade` | 1 |
| 39 | unencrypted | key | valid | right | absent | `Downgrade` | 1 |
| 40 | unencrypted | key | valid | wrong | right | `Downgrade` | 1 |
| 41 | unencrypted | key | valid | wrong | wrong | `Downgrade` | 1 |
| 42 | unencrypted | key | valid | wrong | absent | `Downgrade` | 1 |
| 43 | unencrypted | key | valid | absent | right | `Downgrade` | 1 |
| 44 | unencrypted | key | valid | absent | wrong | `Downgrade` | 1 |
| 45 | unencrypted | key | valid | absent | absent | `Downgrade` | 1 |
| 46 | unencrypted | key | invalid | right | right | `Downgrade` | 1 |
| 47 | unencrypted | key | invalid | right | wrong | `Downgrade` | 1 |
| 48 | unencrypted | key | invalid | right | absent | `Downgrade` | 1 |
| 49 | unencrypted | key | invalid | wrong | right | `Downgrade` | 1 |
| 50 | unencrypted | key | invalid | wrong | wrong | `Downgrade` | 1 |
| 51 | unencrypted | key | invalid | wrong | absent | `Downgrade` | 1 |
| 52 | unencrypted | key | invalid | absent | right | `Downgrade` | 1 |
| 53 | unencrypted | key | invalid | absent | wrong | `Downgrade` | 1 |
| 54 | unencrypted | key | invalid | absent | absent | `Downgrade` | 1 |
| 55 | unencrypted | none | valid | right | right | `Downgrade` | 2 |
| 56 | unencrypted | none | valid | right | wrong | `Downgrade` | 2 |
| 57 | unencrypted | none | valid | right | absent | `Downgrade` | 2 |
| 58 | unencrypted | none | valid | wrong | right | `Downgrade` | 2 |
| 59 | unencrypted | none | valid | wrong | wrong | `Downgrade` | 2 |
| 60 | unencrypted | none | valid | wrong | absent | `Downgrade` | 2 |
| 61 | unencrypted | none | valid | absent | right | `Ok` | 7 |
| 62 | unencrypted | none | valid | absent | wrong | `Downgrade` | 2 |
| 63 | unencrypted | none | valid | absent | absent | `Ok+pin` | 7 |
| 64 | unencrypted | none | invalid | right | right | `Downgrade` | 2 |
| 65 | unencrypted | none | invalid | right | wrong | `Downgrade` | 2 |
| 66 | unencrypted | none | invalid | right | absent | `Downgrade` | 2 |
| 67 | unencrypted | none | invalid | wrong | right | `Downgrade` | 2 |
| 68 | unencrypted | none | invalid | wrong | wrong | `Downgrade` | 2 |
| 69 | unencrypted | none | invalid | wrong | absent | `Downgrade` | 2 |
| 70 | unencrypted | none | invalid | absent | right | `BadMac` | 4 |
| 71 | unencrypted | none | invalid | absent | wrong | `Downgrade` | 2 |
| 72 | unencrypted | none | invalid | absent | absent | `BadMac` | 4 |

Row 63 is the unanchored case ADR-11 F7 accepts: a plain volume with no
credential, no fingerprint and no pin attaches and is pinned as plain.

## Table B: integrity-mode descriptors

Not among the plan's 72 combinations, but needed for M2's check: the
integrity credential of ADR-11 F7 against an `integrity` descriptor, and an
`integrity` descriptor attached without a credential.

| # | descriptor | credential | MAC | fingerprint | pin | result | rule |
|---|---|---|---|---|---|---|---|
| 73 | integrity | integrity | valid | right | right | `Ok` | 7 |
| 74 | integrity | integrity | valid | right | wrong | `WrongKmk` | 6 |
| 75 | integrity | integrity | valid | right | absent | `Ok+pin` | 7 |
| 76 | integrity | integrity | valid | wrong | right | `WrongKmk` | 5 |
| 77 | integrity | integrity | valid | wrong | wrong | `WrongKmk` | 5 |
| 78 | integrity | integrity | valid | wrong | absent | `WrongKmk` | 5 |
| 79 | integrity | integrity | valid | absent | right | `Ok` | 7 |
| 80 | integrity | integrity | valid | absent | wrong | `WrongKmk` | 6 |
| 81 | integrity | integrity | valid | absent | absent | `Ok+pin` | 7 |
| 82 | integrity | integrity | invalid | right | right | `BadMac` | 4 |
| 83 | integrity | integrity | invalid | right | wrong | `BadMac` | 4 |
| 84 | integrity | integrity | invalid | right | absent | `BadMac` | 4 |
| 85 | integrity | integrity | invalid | wrong | right | `BadMac` | 4 |
| 86 | integrity | integrity | invalid | wrong | wrong | `BadMac` | 4 |
| 87 | integrity | integrity | invalid | wrong | absent | `BadMac` | 4 |
| 88 | integrity | integrity | invalid | absent | right | `BadMac` | 4 |
| 89 | integrity | integrity | invalid | absent | wrong | `BadMac` | 4 |
| 90 | integrity | integrity | invalid | absent | absent | `BadMac` | 4 |
| 91 | integrity | none | valid | right | right | `NeedCredential` | 1 |
| 92 | integrity | none | valid | right | wrong | `NeedCredential` | 1 |
| 93 | integrity | none | valid | right | absent | `NeedCredential` | 1 |
| 94 | integrity | none | valid | wrong | right | `NeedCredential` | 1 |
| 95 | integrity | none | valid | wrong | wrong | `NeedCredential` | 1 |
| 96 | integrity | none | valid | wrong | absent | `NeedCredential` | 1 |
| 97 | integrity | none | valid | absent | right | `NeedCredential` | 1 |
| 98 | integrity | none | valid | absent | wrong | `NeedCredential` | 1 |
| 99 | integrity | none | valid | absent | absent | `NeedCredential` | 1 |
| 100 | integrity | none | invalid | right | right | `NeedCredential` | 1 |
| 101 | integrity | none | invalid | right | wrong | `NeedCredential` | 1 |
| 102 | integrity | none | invalid | right | absent | `NeedCredential` | 1 |
| 103 | integrity | none | invalid | wrong | right | `NeedCredential` | 1 |
| 104 | integrity | none | invalid | wrong | wrong | `NeedCredential` | 1 |
| 105 | integrity | none | invalid | wrong | absent | `NeedCredential` | 1 |
| 106 | integrity | none | invalid | absent | right | `NeedCredential` | 1 |
| 107 | integrity | none | invalid | absent | wrong | `NeedCredential` | 1 |
| 108 | integrity | none | invalid | absent | absent | `NeedCredential` | 1 |

## Table C: merged rows

Each row stands for all 18 combinations of MAC × fingerprint × pin. They
are merged because rule 1 decides them before any of those inputs is read.

| # | descriptor | credential | MAC, fingerprint, pin | result | rule | reason for merging |
|---|---|---|---|---|---|---|
| 109 | integrity | key | any (18) | `Downgrade` | 1 | key material means "must be encrypted" |
| 110 | encrypted | integrity | any (18) | `ModeMismatch` | 1 | the request declared an integrity-only volume |
| 111 | unencrypted (`plain`) | integrity | any (18) | `Downgrade` | 1 | the request expects an integrity key the volume lacks |
