namespace Webapp.Models;

using Webapp.Errors;

/// <summary>
/// Application user record (positional record → Class with param variables).
/// </summary>
public record User(string Id, string Email, string Password, string Role);

/// <summary>
/// A validatable user model.
/// </summary>
public class UserModel
{
    public string Email { get; set; }
    public string Password { get; set; }

    public void Validate()
    {
        if (string.IsNullOrEmpty(Email))
        {
            throw new ValidationException("email is required");
        }
    }
}

public enum UserRole
{
    Guest,
    Member,
    Admin
}
