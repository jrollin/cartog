#include "auth/token_claims.h"

struct TokenClaims token_claims_new(const char *user_id, const char *email, const char *role)
{
    struct TokenClaims claims;
    claims.user_id = user_id;
    claims.email = email;
    claims.role = role;
    claims.issued_at = 0;
    return claims;
}
