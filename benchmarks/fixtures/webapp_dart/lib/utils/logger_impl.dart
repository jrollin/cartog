part of 'logger.dart';

/// Severity levels for a [Logger] entry.
enum LogLevel { debug, info, warn, error }

class Logger {
  final String name;
  LogLevel _min = LogLevel.info;

  Logger._(this.name);

  void setLevel(LogLevel level) {
    _min = level;
  }

  void log(LogLevel level, String message) {
    if (level.index < _min.index) return;
    // ignore: avoid_print
    print('[${level.name.toUpperCase()}] $name: $message');
  }

  void info(String message) => log(LogLevel.info, message);
  void error(String message) => log(LogLevel.error, message);
}
