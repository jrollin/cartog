"""Email template management and rendering."""

from typing import Any, Dict, List, Optional

from ...utils.logging import get_logger
from ...exceptions import ValidationError, NotFoundError

_logger = get_logger("services.email.templates")


class TemplateVariable:
    """A variable placeholder within a template."""

    def __init__(self, name: str, required: bool = True, default: Any = None):
        self.name = name
        self.required = required
        self.default = default

    def resolve(self, context: Dict[str, Any]) -> str:
        """Resolve this variable from a context dictionary."""
        value = context.get(self.name, self.default)
        if value is None and self.required:
            raise ValidationError(f"Missing required template variable: {self.name}")
        return str(value) if value is not None else ""


class EmailTemplate:
    """A reusable email template with variable substitution."""

    def __init__(self, name: str, subject: str, body: str):
        self.name = name
        self.subject = subject
        self.body = body
        self.variables: List[TemplateVariable] = []
        self._parse_variables()

    def _parse_variables(self) -> None:
        """Extract variable placeholders from template body."""
        import re

        pattern = r"\{(\w+)\}"
        found = set(re.findall(pattern, self.body))
        found.update(re.findall(pattern, self.subject))

        for var_name in found:
            self.variables.append(TemplateVariable(var_name))

    def render(self, context: Dict[str, Any]) -> Dict[str, str]:
        """Render the template with given context."""
        _logger.info(f"Rendering template: {self.name}")

        # Validate all required variables are present
        for var in self.variables:
            var.resolve(context)

        rendered_subject = self.subject.format(**context)
        rendered_body = self.body.format(**context)

        return {
            "subject": rendered_subject,
            "body": rendered_body,
        }


class TemplateRegistry:
    """Registry of available email templates."""

    def __init__(self):
        self._templates: Dict[str, EmailTemplate] = {}
        self._load_defaults()

    def _load_defaults(self) -> None:
        """Load built-in templates."""
        defaults = [
            EmailTemplate(
                "welcome",
                "Welcome to {app_name}!",
                "Hello {name},\n\nWelcome to {app_name}! We're glad to have you.\n\nBest,\nThe Team",
            ),
            EmailTemplate(
                "password_reset",
                "Password Reset for {app_name}",
                "Hi {name},\n\nClick the link below to reset your password:\n{reset_link}\n\nThis link expires in {expiry_hours} hours.",
            ),
            EmailTemplate(
                "payment_confirmation",
                "Payment Received - {amount} {currency}",
                "Dear {name},\n\nWe received your payment of {amount} {currency}.\nTransaction ID: {transaction_id}\n\nThank you!",
            ),
            EmailTemplate(
                "account_locked",
                "Account Security Alert",
                "Hi {name},\n\nYour account has been locked due to {reason}.\nPlease contact support at {support_email}.",
            ),
        ]
        for tmpl in defaults:
            self._templates[tmpl.name] = tmpl
            _logger.info(f"Loaded template: {tmpl.name}")

    def get(self, name: str) -> EmailTemplate:
        """Retrieve a template by name."""
        template = self._templates.get(name)
        if not template:
            raise NotFoundError("EmailTemplate", name)
        return template

    def register(self, template: EmailTemplate) -> None:
        """Register a new template."""
        self._templates[template.name] = template
        _logger.info(f"Registered template: {template.name}")

    def list_templates(self) -> List[str]:
        """Return names of all registered templates."""
        return list(self._templates.keys())
