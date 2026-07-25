#ifndef AUTH_TOKEN_CLAIMS_H
#define AUTH_TOKEN_CLAIMS_H

/// Decoded claims carried by an authentication token.
struct TokenClaims {
    const char *user_id;
    const char *email;
    const char *role;
    long issued_at;
};

struct TokenClaims token_claims_new(const char *user_id, const char *email, const char *role);

#endif /* AUTH_TOKEN_CLAIMS_H */
