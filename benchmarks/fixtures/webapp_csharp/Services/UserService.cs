namespace Webapp.Services;

using Webapp.Database;
using Webapp.Util;

/// <summary>
/// User CRUD operations backed by the database.
/// </summary>
public class UserService : BaseService
{
    private static readonly Logger Log = Logger.GetLogger("services.user");
    private readonly DatabaseConnection _db;

    public UserService(DatabaseConnection db) : base("user")
    {
        _db = db;
    }

    public void CreateUser(string email)
    {
        Log.Info($"Creating user: {email}");
        _db.ExecuteQuery("INSERT INTO users");
    }
}
