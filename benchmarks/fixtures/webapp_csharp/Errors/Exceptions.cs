namespace Webapp.Errors;

using System;

/// <summary>
/// Base class for all application exceptions.
/// </summary>
public class AppException : Exception
{
    public AppException(string message) : base(message) { }
}

public class ValidationException : AppException
{
    public ValidationException(string message) : base(message) { }
}

public class AuthenticationException : AppException
{
    public AuthenticationException(string message) : base(message) { }
}

public class TokenException : AppException
{
    public TokenException(string message) : base(message) { }
}

public class ExpiredTokenException : TokenException
{
    public ExpiredTokenException(string message) : base(message) { }
}
