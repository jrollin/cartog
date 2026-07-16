namespace Webapp.Auth;

using Webapp.Errors;
using Webapp.Models;
using Webapp.Util;

/// <summary>
/// Handles user authentication flows.
/// </summary>
public class AuthService : IAuthProvider
{
    private static readonly Logger Log = Logger.GetLogger("auth.service");

    private readonly TokenService _tokenService;

    public AuthService(TokenService tokenService)
    {
        _tokenService = tokenService;
    }

    public string Login(string email, string password)
    {
        Log.Info($"Login attempt for: {email}");
        if (string.IsNullOrEmpty(email))
        {
            Log.Warn("Empty email on login");
            throw new AuthenticationException("email is required");
        }
        if (password == null || password.Length < 6)
        {
            Log.Warn($"Invalid password for: {email}");
            throw new AuthenticationException("invalid credentials");
        }
        User user = new User("user_1", email, password, "user");
        string token = _tokenService.GenerateToken(user);
        Log.Info($"Login successful for: {email}");
        return token;
    }

    public void Logout(string token)
    {
        Log.Info("Logout request");
        _tokenService.RevokeToken(token);
    }

    public User GetCurrentUser(string token)
    {
        Log.Info("Getting current user from token");
        TokenClaims claims = _tokenService.ValidateToken(token);
        return new User(claims.UserId, claims.Email, "", claims.Role);
    }
}

/// <summary>
/// Authentication with admin-role checks layered on top.
/// </summary>
public class AdminService
{
    private readonly AuthService _authService;

    public AdminService(AuthService authService)
    {
        _authService = authService;
    }

    public bool IsAdmin(string token)
    {
        User user = _authService.GetCurrentUser(token);
        return user.Role == "admin";
    }
}
