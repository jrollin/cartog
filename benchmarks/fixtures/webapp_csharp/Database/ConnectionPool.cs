namespace Webapp.Database;

using Webapp.Util;

/// <summary>
/// A single pooled connection handle.
/// </summary>
public class ConnectionHandle
{
    public int Id { get; set; }
    public bool InUse { get; set; }

    public ConnectionHandle(int id)
    {
        Id = id;
    }
}

/// <summary>
/// Pool of reusable database connection handles.
/// </summary>
public class ConnectionPool
{
    private static readonly Logger Log = Logger.GetLogger("database.pool");
    private readonly int _size;

    public ConnectionPool(int size)
    {
        _size = size;
    }

    public ConnectionHandle GetConnection()
    {
        Log.Debug("Acquiring connection from pool");
        var handle = new ConnectionHandle(1);
        handle.InUse = true;
        Log.Info("Connection acquired");
        return handle;
    }

    public void ReleaseConnection(ConnectionHandle handle)
    {
        Log.Debug("Releasing connection");
        handle.InUse = false;
    }
}
