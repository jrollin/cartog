namespace Webapp.Routes;

using Webapp.Database;
using Webapp.Services;
using Webapp.Util;

/// <summary>
/// HTTP route handlers for authentication endpoints.
/// </summary>
public class AuthRoutes
{
    private static readonly Logger Log = Logger.GetLogger("routes.auth");

    public string LoginHandler(string email, string password)
    {
        Log.Info("Handling login request");
        var db = new DatabaseConnection("localhost", 5432, "app");
        var authSvc = new AuthenticationService(db);
        return authSvc.Authenticate(email, password);
    }
}
