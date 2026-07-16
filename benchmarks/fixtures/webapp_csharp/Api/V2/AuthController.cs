namespace Webapp.Api.V2;

using Webapp.Routes;
using Webapp.Util;
using Webapp.Validators;

/// <summary>
/// v2 authentication HTTP controller.
/// </summary>
public class AuthController
{
    private static readonly Logger Log = Logger.GetLogger("api.v2.auth");
    private readonly UserValidator _validator = new UserValidator();
    private readonly AuthRoutes _routes = new AuthRoutes();

    public string HandleLogin(string email, string password)
    {
        Log.Info("v2 login");
        _validator.Validate(email);
        return _routes.LoginHandler(email, password);
    }
}
