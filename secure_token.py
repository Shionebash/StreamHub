#!/usr/bin/env python3
"""
Secure token storage for StreamHub.
Priority: OS keyring → Fernet-encrypted file → plaintext fallback.
"""

import os
import sys

KEYRING_SERVICE = "StreamHub"
LEGACY_KEYRING_SERVICE = "TwitchGrid"
KEYRING_USER    = "oauth_token"


def _runtime_dir():
    base = os.path.dirname(os.path.abspath(__file__))
    return os.path.join(base, "runtime app")


def _key_file():
    return os.path.join(_runtime_dir(), "keyfile.key")


def _enc_file():
    return os.path.join(_runtime_dir(), "token.enc")


# ── KEYRING ──────────────────────────────────────────────────────────

def _keyring_set(token):
    try:
        import keyring
        keyring.set_password(KEYRING_SERVICE, KEYRING_USER, token)
        return True
    except Exception:
        return False


def _keyring_get():
    try:
        import keyring
        tok = keyring.get_password(KEYRING_SERVICE, KEYRING_USER) or ""
        if tok:
            return tok
        legacy = keyring.get_password(LEGACY_KEYRING_SERVICE, KEYRING_USER) or ""
        if legacy:
            try:
                keyring.set_password(KEYRING_SERVICE, KEYRING_USER, legacy)
            except Exception:
                pass
        return legacy
    except Exception:
        return ""


def _keyring_delete():
    try:
        import keyring
        keyring.delete_password(KEYRING_SERVICE, KEYRING_USER)
        try:
            keyring.delete_password(LEGACY_KEYRING_SERVICE, KEYRING_USER)
        except Exception:
            pass
    except Exception:
        pass


# ── FERNET ───────────────────────────────────────────────────────────

def _get_or_create_key():
    kf = _key_file()
    if os.path.exists(kf):
        with open(kf, "rb") as f:
            return f.read()
    try:
        from cryptography.fernet import Fernet
        key = Fernet.generate_key()
        os.makedirs(_runtime_dir(), exist_ok=True)
        with open(kf, "wb") as f:
            f.write(key)
        if sys.platform != "win32":
            os.chmod(kf, 0o600)
        return key
    except Exception:
        return None


def _fernet_set(token):
    try:
        from cryptography.fernet import Fernet
        key = _get_or_create_key()
        if not key:
            return False
        encrypted = Fernet(key).encrypt(token.encode())
        ef = _enc_file()
        os.makedirs(_runtime_dir(), exist_ok=True)
        with open(ef, "wb") as f:
            f.write(encrypted)
        if sys.platform != "win32":
            os.chmod(ef, 0o600)
        return True
    except Exception:
        return False


def _fernet_get():
    try:
        from cryptography.fernet import Fernet
        ef = _enc_file()
        if not os.path.exists(ef):
            return ""
        key = _get_or_create_key()
        if not key:
            return ""
        with open(ef, "rb") as f:
            data = f.read()
        return Fernet(key).decrypt(data).decode()
    except Exception:
        return ""


def _fernet_delete():
    try:
        ef = _enc_file()
        if os.path.exists(ef):
            os.remove(ef)
    except Exception:
        pass


# ── PUBLIC API ────────────────────────────────────────────────────────

def store_token(token):
    """Store token securely. Returns backend: 'keyring', 'fernet', or 'none'."""
    token = str(token or "").strip()
    if not token:
        return "none"
    if _keyring_set(token):
        return "keyring"
    if _fernet_set(token):
        return "fernet"
    return "none"


def load_token():
    """Load token from secure storage. Returns (token, backend)."""
    tok = _keyring_get()
    if tok:
        return tok, "keyring"
    tok = _fernet_get()
    if tok:
        return tok, "fernet"
    return "", "none"


def delete_token():
    """Remove token from all secure backends."""
    _keyring_delete()
    _fernet_delete()


def migrate_from_plaintext(config_token):
    """Migrate plaintext token from config.json to secure storage.
    Returns True if the token is now stored securely (remove from config)."""
    if not config_token:
        return False
    existing, _ = load_token()
    if existing:
        return True
    return store_token(config_token) != "none"
