export class TokenError extends Error {}

export interface TokenClaims {
  userId: string;
  expiresAt: number;
}

export function generateToken(userId: string): string {
  const claims: TokenClaims = { userId, expiresAt: nowPlus(3600) };
  return encodeClaims(claims);
}

export function validateToken(token: string): TokenClaims {
  const claims = decodeClaims(token);
  if (claims.expiresAt < nowPlus(0)) {
    throw new TokenError("token expired");
  }
  return claims;
}

function encodeClaims(claims: TokenClaims): string {
  return btoa(JSON.stringify(claims));
}

function decodeClaims(token: string): TokenClaims {
  return JSON.parse(atob(token)) as TokenClaims;
}

function nowPlus(seconds: number): number {
  return Math.floor(Date.now() / 1000) + seconds;
}
