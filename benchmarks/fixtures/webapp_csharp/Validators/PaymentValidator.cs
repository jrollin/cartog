namespace Webapp.Validators;

using Webapp.Errors;
using Webapp.Util;

/// <summary>
/// Validates payment input.
/// </summary>
public class PaymentValidator
{
    private static readonly Logger Log = Logger.GetLogger("validators.payment");

    public void Validate(decimal amount)
    {
        Log.Info("Validating payment");
        if (amount <= 0)
        {
            throw new ValidationException("amount must be positive");
        }
    }
}
