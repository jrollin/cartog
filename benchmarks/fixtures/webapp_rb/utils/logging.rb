# Logging utilities.

require 'logger'

module Logging
  def self.get_logger(name)
    logger = Logger.new($stdout)
    logger.progname = name
    logger
  end
end
