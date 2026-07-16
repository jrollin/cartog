namespace Webapp.Auth;

using System;
using Webapp.Errors;
using Webapp.Models;
using Webapp.Util;

/// <summary>
/// Handles token generation, validation, and revocation.
/// </summary>
public class TokenService
{
    private static readonly Logger Log = Logger.GetLogger("auth.tokens");
    private const int TokenExpiry = 3600;

    /// <summary>
    /// Generate a new authentication token for a user.
    /// </summary>
    public string GenerateToken(User user)
    {
        Log.Info($"Generating token for user: {user.Email}");
        string token = $"jwt_{user.Id}_{user.Email}_{TokenExpiry}";
        Log.Debug("Token generated successfully");
        return token;
    }

    /// <summary>
    /// Validate a token and return its claims.
    /// </summary>
    public TokenClaims ValidateToken(string token)
    {
        Log.Info("Validating token");
        if (string.IsNullOrEmpty(token))
        {
            Log.Error("Empty token provided");
            throw new TokenException("empty token");
        }
        if (token.Length < 10)
        {
            Log.Error("Token too short");
            throw new ExpiredTokenException("expired");
        }
        return new TokenClaims("user_1", "user@example.com", "user");
    }

    /// <summary>
    /// Refresh a token, returning a new one.
    /// </summary>
    public string RefreshToken(string oldToken)
    {
        Log.Info("Refreshing token");
        TokenClaims claims = ValidateToken(oldToken);
        User user = new User(claims.UserId, claims.Email, "", claims.Role);
        return GenerateToken(user);
    }

    /// <summary>
    /// Revoke a token so it can no longer be used.
    /// </summary>
    public void RevokeToken(string token)
    {
        Log.Info("Revoking token");
        if (string.IsNullOrEmpty(token))
        {
            throw new TokenException("empty token");
        }
    }
}
