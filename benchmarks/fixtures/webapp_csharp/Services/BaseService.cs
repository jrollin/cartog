namespace Webapp.Services;

using Webapp.Util;

/// <summary>
/// Common base for all application services.
/// </summary>
public abstract class BaseService
{
    private static readonly Logger Log = Logger.GetLogger("services.base");
    private readonly string _serviceName;
    private bool _initialized;

    protected BaseService(string serviceName)
    {
        _serviceName = serviceName;
        _initialized = false;
    }

    public string Name => _serviceName;

    public void Initialize()
    {
        Log.Info($"Initializing service: {_serviceName}");
        _initialized = true;
    }

    protected void RequireInitialized()
    {
        if (!_initialized)
        {
            Log.Warn($"{_serviceName} is not initialized");
        }
    }
}
