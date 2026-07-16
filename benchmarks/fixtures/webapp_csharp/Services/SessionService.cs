namespace Webapp.Services;

using Webapp.Database;
using Webapp.Util;

/// <summary>
/// Manages user sessions backed by the database.
/// </summary>
public class SessionService : BaseService
{
    private static readonly Logger Log = Logger.GetLogger("services.session");
    private readonly DatabaseConnection _db;

    public SessionService(DatabaseConnection db) : base("session")
    {
        _db = db;
    }

    public void Create(string token)
    {
        Log.Info("Creating session");
        _db.ExecuteQuery("INSERT INTO sessions");
    }
}
