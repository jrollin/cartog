namespace Webapp.Services;

using System.Collections.Generic;
using Webapp.Auth;
using Webapp.Database;
using Webapp.Models;
using Webapp.Util;

/// <summary>
/// Orchestrates the full authentication workflow.
///
/// Deep call chain entry point:
/// Authenticate() -> Login() -> GenerateToken() -> ExecuteQuery() -> GetConnection()
/// </summary>
public class AuthenticationService : BaseService
{
    private static readonly Logger Log = Logger.GetLogger("services.authentication");

    private readonly AuthService _authService;
    private readonly DatabaseConnection _db;

    public AuthenticationService(DatabaseConnection db) : base("authentication")
    {
        Log.Info("Creating AuthenticationService");
        _authService = new AuthService(new TokenService());
        _db = db;
    }

    /// <summary>
    /// Perform the full authentication flow.
    /// </summary>
    public string Authenticate(string email, string password)
    {
        RequireInitialized();
        Log.Info($"Authenticating user: {email}");

        string token = _authService.Login(email, password);

        var session = new Dictionary<string, object>
        {
            ["token"] = token,
            ["email"] = email
        };
        _db.Insert("sessions", session);

        Log.Info($"Authentication successful for: {email}");
        return token;
    }

    public void Logout(string token)
    {
        Log.Info("Logging out");
        _authService.Logout(token);
    }

    public User GetCurrentUser(string token)
    {
        Log.Info("Getting current user");
        return _authService.GetCurrentUser(token);
    }
}
