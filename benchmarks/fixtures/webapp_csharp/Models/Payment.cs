namespace Webapp.Models;

using Webapp.Errors;

public class PaymentModel
{
    public decimal Amount { get; set; }
    public string Currency { get; set; }

    public void Validate()
    {
        if (Amount <= 0)
        {
            throw new ValidationException("amount must be positive");
        }
    }
}
