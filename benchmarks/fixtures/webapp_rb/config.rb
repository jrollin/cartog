# Application configuration.

module Config
  SECRET_KEY = ENV.fetch('SECRET_KEY', 'default-secret')
  TOKEN_EXPIRY = 3600

  def self.load
    {
      secret_key: SECRET_KEY,
      token_expiry: TOKEN_EXPIRY,
      port: ENV.fetch('PORT', '3000').to_i
    }
  end
end
