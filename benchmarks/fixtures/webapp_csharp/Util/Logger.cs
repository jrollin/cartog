namespace Webapp.Util;

using System;

/// <summary>
/// Simple structured logger for a named component.
/// </summary>
public class Logger
{
    private readonly string _name;

    public Logger(string name)
    {
        _name = name;
    }

    /// <summary>
    /// Factory method: creates a Logger for the given component name.
    /// </summary>
    public static Logger GetLogger(string name)
    {
        return new Logger(name);
    }

    public void Info(string message)
    {
        Console.WriteLine($"[INFO] [{_name}] {message}");
    }

    public void Warn(string message)
    {
        Console.WriteLine($"[WARN] [{_name}] {message}");
    }

    public void Error(string message)
    {
        Console.WriteLine($"[ERROR] [{_name}] {message}");
    }

    public void Debug(string message)
    {
        Console.WriteLine($"[DEBUG] [{_name}] {message}");
    }
}
