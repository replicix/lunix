#!/usr/bin/env python3
"""Independent cross-check of the published vectors (P0.8).

Implements BLAKE3 (single-chunk inputs only) from the specification, and uses
Python's `cryptography` package for AES-256-GCM, ChaCha20-Poly1305 (for
XChaCha via a hand-written HChaCha20) and Argon2id, plus hmac/hashlib for
HKDF-SHA-256. Nothing here shares code with the Rust implementation.

usage: crosscheck.py kat-vectors.json
"""
import hashlib, hmac, json, struct, sys

M = 0xFFFFFFFF
IV = [0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19]
PERM = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8]
CHUNK_START, CHUNK_END, ROOT, KEYED_HASH, DK_CONTEXT, DK_MATERIAL = 1, 2, 8, 16, 32, 64


def rotr(x, n):
    return ((x >> n) | (x << (32 - n))) & M


def g(s, a, b, c, d, mx, my):
    s[a] = (s[a] + s[b] + mx) & M
    s[d] = rotr(s[d] ^ s[a], 16)
    s[c] = (s[c] + s[d]) & M
    s[b] = rotr(s[b] ^ s[c], 12)
    s[a] = (s[a] + s[b] + my) & M
    s[d] = rotr(s[d] ^ s[a], 8)
    s[c] = (s[c] + s[d]) & M
    s[b] = rotr(s[b] ^ s[c], 7)


def compress(cv, m, counter, blen, flags):
    s = list(cv) + IV[:4] + [counter & M, counter >> 32, blen, flags]
    m = list(m)
    for r in range(7):
        g(s, 0, 4, 8, 12, m[0], m[1]); g(s, 1, 5, 9, 13, m[2], m[3])
        g(s, 2, 6, 10, 14, m[4], m[5]); g(s, 3, 7, 11, 15, m[6], m[7])
        g(s, 0, 5, 10, 15, m[8], m[9]); g(s, 1, 6, 11, 12, m[10], m[11])
        g(s, 2, 7, 8, 13, m[12], m[13]); g(s, 3, 4, 9, 14, m[14], m[15])
        if r != 6:
            m = [m[i] for i in PERM]
    for i in range(8):
        s[i] ^= s[i + 8]
        s[i + 8] ^= cv[i]
    return s


def blake3_single(key_words, flags, data):
    assert len(data) <= 1024
    blocks = [data[i:i + 64] for i in range(0, len(data), 64)] or [b""]
    cv = list(key_words)
    for i, b in enumerate(blocks):
        f = flags
        if i == 0: f |= CHUNK_START
        last = i == len(blocks) - 1
        if last: f |= CHUNK_END | ROOT
        words = struct.unpack("<16I", b.ljust(64, b"\0"))
        out = compress(cv, words, 0, len(b), f)
        cv = out[:8]
    return struct.pack("<8I", *cv)


def b3_hash(data): return blake3_single(IV, 0, data)
def b3_keyed(key, data): return blake3_single(struct.unpack("<8I", key), KEYED_HASH, data)
def b3_derive_key(ctx, material):
    ck = blake3_single(IV, DK_CONTEXT, ctx.encode())
    return blake3_single(struct.unpack("<8I", ck), DK_MATERIAL, material)


def hkdf_expand32(prk, info):
    return hmac.new(prk, info + b"\x01", hashlib.sha256).digest()


def hchacha20(key, nonce16):
    def qr(s, a, b, c, d):
        s[a] = (s[a] + s[b]) & M; s[d] = rotl(s[d] ^ s[a], 16)
        s[c] = (s[c] + s[d]) & M; s[b] = rotl(s[b] ^ s[c], 12)
        s[a] = (s[a] + s[b]) & M; s[d] = rotl(s[d] ^ s[a], 8)
        s[c] = (s[c] + s[d]) & M; s[b] = rotl(s[b] ^ s[c], 7)
    def rotl(x, n): return ((x << n) | (x >> (32 - n))) & M
    s = [0x61707865, 0x3320646E, 0x79622D32, 0x6B206574] + list(struct.unpack("<8I", key)) + list(struct.unpack("<4I", nonce16))
    for _ in range(10):
        qr(s, 0, 4, 8, 12); qr(s, 1, 5, 9, 13); qr(s, 2, 6, 10, 14); qr(s, 3, 7, 11, 15)
        qr(s, 0, 5, 10, 15); qr(s, 1, 6, 11, 12); qr(s, 2, 7, 8, 13); qr(s, 3, 4, 9, 14)
    return struct.pack("<8I", *(s[0:4] + s[12:16]))


def main():
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM, ChaCha20Poly1305
    from cryptography.hazmat.primitives.kdf.argon2 import Argon2id
    v = json.load(open(sys.argv[1]))
    H = bytes.fromhex
    fails = 0

    def check(name, got, want):
        nonlocal fails
        ok = got == want
        print(("PASS " if ok else "FAIL ") + name)
        if not ok:
            fails += 1
            print("   python:", got.hex() if isinstance(got, bytes) else got, "\n   rust:  ", want.hex() if isinstance(want, bytes) else want)

    check("blake3 hash('') official", b3_hash(b""), H("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"))
    dek, kmk, fam, cid, salt = (H(v[k]) for k in ("dek", "kmk", "family", "chunk_id", "salt"))
    pl = H(v["payload_raw64"])
    check("key_v0", b3_derive_key("lunix extent v0", dek + cid), H(v["key_v0"]))
    check("key_adr", b3_derive_key("lunix extent v1", dek + cid + salt), H(v["key_adr"]))
    info = H(v["info_v2"])
    exp_info = b"lunix extent v2\0" + fam + bytes([0]) + struct.pack("<I", 7) + bytes([0, 0]) + struct.pack("<I", 64) + bytes([32]) + cid + salt
    check("info_v2 layout", info, exp_info)
    check("key_hardened", b3_keyed(dek, info), H(v["key_hardened"]))
    check("key_fips", hkdf_expand32(dek, info), H(v["key_fips"]))
    for name, keyname in (("adr", "key_adr"), ("hardened", "key_hardened"), ("fips_kdf_zero_nonce", "key_fips")):
        ct = AESGCM(H(v[keyname])).encrypt(b"\0" * 12, pl, H(v["aad_" + name]))
        check("extent body " + name, ct, H(v["body_" + name]))
    # official AES-256-GCM vector (McGrew/Viega test case 14): zero key, zero IV
    ct = AESGCM(b"\0" * 32).encrypt(b"\0" * 12, b"\0" * 16, b"")
    check("AES-256-GCM test case 14", ct, H("cea7403d4d606b6e074ec5d3baf39d18d0d1c8a799996bf0265b98b5d48ab919"))
    msg = lambda tag: b"lunix kdf v1\0" + fam + tag
    pur = {"addressing": b"\x01", "dek0": b"\x02" + struct.pack("<I", 0), "dek1": b"\x02" + struct.pack("<I", 1),
           "meta_commit": b"\x03\x01", "meta_lease": b"\x03\x02", "volume_mac": b"\x04", "pin": b"\x05"}
    for n, t in pur.items():
        check("purpose " + n, b3_keyed(kmk, msg(t)), H(v["purpose_" + n]))
        check("purpose fips " + n, hkdf_expand32(kmk, msg(t)), H(v["purpose_fips_" + n]))
    # XChaCha20-Poly1305 = ChaCha20-Poly1305(HChaCha20(key, n[:16]), 0^4 || n[16:])
    n = H(v["xchacha_nonce"])
    sub = hchacha20(dek, n[:16])
    ct = ChaCha20Poly1305(sub).encrypt(b"\0\0\0\0" + n[16:], b"lunix xchacha vector", b"aad-1")
    check("XChaCha20-Poly1305", ct, H(v["xchacha_ct"]))
    kek = Argon2id(salt=b"\x11" * 16, length=32, iterations=2, lanes=1, memory_cost=19456).derive(b"correct horse battery staple")
    check("argon2id slot KEK", kek, H(v["argon2id_kek_m19456_t2_p1"]))
    # RFC 9106 section 5.3 Argon2id vector
    t = Argon2id(salt=b"\x02" * 16, length=32, iterations=3, lanes=4, memory_cost=32, ad=b"\x04" * 12, secret=b"\x03" * 8).derive(b"\x01" * 32)
    check("argon2id RFC 9106 vector (python lib)", t, H("0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659"))
    print("\nall independent checks passed" if not fails else f"\n{fails} FAILED")
    sys.exit(1 if fails else 0)


main()
