# Token validation and generation.

require_relative '../models/user'
require_relative '../models/session'
require_relative '../config'

class TokenError < StandardError
  # Base exception for token errors.
end

class ExpiredTokenError < TokenError
  # Raised when a token has expired.
end

class InvalidScopeError < TokenError
  # Raised when token scope is insufficient.
end

# Generate a new authentication token for a user.
def generate_token(user, expires_in = Config::TOKEN_EXPIRY)
  payload = "#{user.id}:#{Time.now.utc.iso8601}"
  token = Digest::SHA256.hexdigest("#{payload}:#{Config::SECRET_KEY}")
  Session.create(user: user, token: token, expires_in: expires_in)
  token
end

# Validate a token and return the associated user.
def validate_token(token)
  session = lookup_session(token)
  raise TokenError, 'Invalid token' if session.nil?
  raise ExpiredTokenError, 'Token has expired' if session.expired?

  session.user
end

# Look up a session by its token.
def lookup_session(token)
  Session.find_by_token(token)
end

# Refresh an expiring token.
def refresh_token(old_token)
  user = validate_token(old_token)
  revoke_token(old_token)
  generate_token(user)
end

# Revoke a token, invalidating the session.
def revoke_token(token)
  session = lookup_session(token)
  return false unless session

  session.delete
  true
end

# Revoke all tokens for a user.
def revoke_all_tokens(user)
  sessions = Session.find_all_by_user(user)
  count = 0
  sessions.each do |session|
    session.delete
    count += 1
  end
  count
end
