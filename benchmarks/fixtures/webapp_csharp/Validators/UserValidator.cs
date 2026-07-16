namespace Webapp.Validators;

using Webapp.Errors;
using Webapp.Util;

/// <summary>
/// Validates user-facing input.
/// </summary>
public class UserValidator
{
    private static readonly Logger Log = Logger.GetLogger("validators.user");

    public void Validate(string email)
    {
        Log.Info("Validating user input");
        if (string.IsNullOrEmpty(email))
        {
            throw new ValidationException("email is required");
        }
    }

    public void ValidateLogin(string email, string password)
    {
        Validate(email);
        if (password == null || password.Length < 6)
        {
            throw new ValidationException("password too short");
        }
    }
}
