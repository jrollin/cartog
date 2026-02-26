"""Cryptographic utilities."""

import hashlib
import os


def generate_salt() -> str:
    """Generate a random salt for password hashing."""
    return os.urandom(32).hex()


def hash_password(password: str, salt: str = None) -> str:
    """Hash a password with a salt."""
    if salt is None:
        salt = generate_salt()
    hashed = hashlib.pbkdf2_hmac("sha256", password.encode(), salt.encode(), 100000)
    return f"{salt}${hashed.hex()}"


def verify_password(password: str, stored_hash: str) -> bool:
    """Verify a password against a stored hash."""
    parts = stored_hash.split("$")
    if len(parts) != 2:
        return False
    salt = parts[0]
    expected = hash_password(password, salt)
    return expected == stored_hash
