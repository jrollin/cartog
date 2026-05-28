/// Library that exercises the `part` / `part of` directive pair.
library webapp_dart.utils.logger;

part 'logger_impl.dart';

/// Public entry point — delegates to the part file.
Logger getLogger(String name) => Logger._(name);
