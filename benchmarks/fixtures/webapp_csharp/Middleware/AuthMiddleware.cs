namespace Webapp.Middleware;

using Webapp.Auth;
using Webapp.Util;

/// <summary>
/// Middleware that authenticates a request by validating its token.
/// </summary>
public class AuthMiddleware
{
    private static readonly Logger Log = Logger.GetLogger("middleware.auth");
    private readonly TokenService _tokenService;

    public AuthMiddleware(TokenService tokenService)
    {
        _tokenService = tokenService;
    }

    public bool Authenticate(string token)
    {
        Log.Info("Authenticating request");
        TokenClaims claims = _tokenService.ValidateToken(token);
        return claims != null;
    }
}
