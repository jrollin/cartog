namespace Webapp.Database;

using System.Collections.Generic;
using Webapp.Util;

/// <summary>
/// Represents a single database connection with query execution support.
/// </summary>
public class DatabaseConnection
{
    private static readonly Logger Log = Logger.GetLogger("database.connection");
    private readonly ConnectionPool _pool;

    public DatabaseConnection(string host, int port, string database)
    {
        Log.Info($"Creating database connection: {host}:{port}/{database}");
        _pool = new ConnectionPool(10);
        Log.Info("Database connection established");
    }

    /// <summary>
    /// Execute a query and return results.
    /// </summary>
    public List<Dictionary<string, object>> ExecuteQuery(string query)
    {
        Log.Info($"Executing query: {query}");
        ConnectionHandle handle = _pool.GetConnection();
        Log.Debug($"Query executed on connection #{handle.Id}");
        _pool.ReleaseConnection(handle);
        return new List<Dictionary<string, object>>();
    }

    public string Insert(string table, Dictionary<string, object> data)
    {
        Log.Info($"Insert into table: {table}");
        ExecuteQuery($"INSERT INTO {table} VALUES (?)");
        return "generated_id";
    }
}

/// <summary>
/// Generic repository over a table (D3: generics → clean base name Repository).
/// </summary>
public class Repository<T> where T : class
{
    private readonly DatabaseConnection _db;

    public Repository(DatabaseConnection db)
    {
        _db = db;
    }

    public void Save(T entity)
    {
        _db.ExecuteQuery("INSERT");
    }
}
