"""Logging utilities."""

from config import LOG_LEVEL


class Logger:
    """Simple logger with level filtering."""

    def __init__(self, name: str, level: str = LOG_LEVEL):
        self.name = name
        self.level = level

    def info(self, message: str):
        """Log an info message."""
        self._write("INFO", message)

    def warning(self, message: str):
        """Log a warning message."""
        self._write("WARNING", message)

    def error(self, message: str):
        """Log an error message."""
        self._write("ERROR", message)

    def _write(self, level: str, message: str):
        """Write a log entry."""
        print(f"[{level}] {self.name}: {message}")


def get_logger(name: str) -> Logger:
    """Get a logger instance for the given name."""
    return Logger(name)
