namespace Webapp.Auth;

/// <summary>
/// Decoded claims carried by an authentication token.
/// </summary>
public class TokenClaims
{
    public string UserId { get; set; }
    public string Email { get; set; }
    public string Role { get; set; }

    public TokenClaims(string userId, string email, string role)
    {
        UserId = userId;
        Email = email;
        Role = role;
    }
}
