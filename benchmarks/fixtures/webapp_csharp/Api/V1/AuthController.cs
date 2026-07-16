namespace Webapp.Api.V1;

using Webapp.Routes;
using Webapp.Util;
using Webapp.Validators;

/// <summary>
/// v1 authentication HTTP controller.
/// </summary>
public class AuthController
{
    private static readonly Logger Log = Logger.GetLogger("api.v1.auth");
    private readonly UserValidator _validator = new UserValidator();
    private readonly AuthRoutes _routes = new AuthRoutes();

    public string HandleLogin(string email, string password)
    {
        Log.Info("v1 login");
        _validator.Validate(email);
        return _routes.LoginHandler(email, password);
    }
}
