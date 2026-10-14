# SPDX-License-Identifier: Apache-2.0
"""Runs in the guest: the kernel's crypto drivers through AF_ALG, 64 KiB
requests (a digest per MiB) for one second each, GB/s. Driver names pin an implementation;
plain names take what the kernel picked (the highest priority). Prints
`label GB/s` or `label error`."""

import os
import socket
import time

BUF = bytes(65536)
CASES = [
    ("skcipher", "xts-aes-ce", 64),
    ("skcipher", "xts-aes-neonbs", 64),
    ("skcipher", "xts(ecb(aes-generic))", 64),
    ("skcipher", "ctr-aes-ce", 16),
    ("aead", "gcm-aes-ce", 16),
    ("aead", "gcm_base(ctr(aes-generic),ghash-generic)", 16),
    ("hash", "sha256", 0),
    ("hash", "sha512", 0),
    ("hash", "sha3-256-ce", 0),
    ("hash", "sha3-256-generic", 0),
    ("hash", "crc32c", 0),
]


def rate(kind, name, keylen):
    s = socket.socket(socket.AF_ALG, socket.SOCK_SEQPACKET, 0)
    s.bind((kind, name))
    if keylen:
        s.setsockopt(socket.SOL_ALG, socket.ALG_SET_KEY, os.urandom(keylen))
    if kind == "aead":
        s.setsockopt(socket.SOL_ALG, socket.ALG_SET_AEAD_AUTHSIZE, None, 16)
    op, _ = s.accept()
    n, start = 0, time.monotonic()
    while time.monotonic() - start < 1:
        if kind == "hash":
            # One digest per MiB, so the request overhead stays small.
            for _ in range(15):
                op.send(BUF, socket.MSG_MORE)
            op.send(BUF)
            op.recv(64)
            n += 15 * len(BUF)
        elif kind == "aead":
            op.sendmsg_afalg([BUF], op=socket.ALG_OP_ENCRYPT, iv=bytes(12), assoclen=0)
            op.recv(len(BUF) + 16)
        else:
            op.sendmsg_afalg([BUF], op=socket.ALG_OP_ENCRYPT, iv=bytes(16))
            op.recv(len(BUF))
        n += len(BUF)
    op.close()
    s.close()
    return n / (time.monotonic() - start) / 1e9


for kind, name, keylen in CASES:
    try:
        print(f"{name} {rate(kind, name, keylen):.2f}")
    except OSError as e:
        print(f"{name} error({e.errno})")
