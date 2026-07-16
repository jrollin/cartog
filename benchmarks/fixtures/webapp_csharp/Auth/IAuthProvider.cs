namespace Webapp.Auth;

/// <summary>
/// Contract for authentication providers.
/// </summary>
public interface IAuthProvider
{
    string Login(string email, string password);
    void Logout(string token);
}
