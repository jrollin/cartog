# High-level authentication service.

require_relative '../utils/helpers'
require_relative '../auth/service'
require_relative '../auth/tokens'
require_relative '../database/connection'
require_relative '../database/queries'
require_relative '../events/dispatcher'
require_relative '../exceptions'

# Orchestrates authentication flows.
class AuthenticationService < BaseService
  def initialize(db, events)
    super()
    @auth = AuthService.new(db)
    @users = UserQueries.new(db)
    @sessions = SessionQueries.new(db)
    @events = events
  end

  # Authenticate a user — main entry point for login flow.
  def authenticate(email, password, ip = 'unknown')
    _log("Authentication attempt for #{email}")
    clean_email = sanitize_input(email)
    raise ValidationError.new('Email is required', field: :email) if clean_email.empty?

    begin
      token = @auth.login(clean_email, password)
      unless token
        @events.emit('auth.login_failed', { email: clean_email, ip: ip })
        raise AuthenticationError.new('Invalid credentials')
      end
      @events.emit('auth.login_success', { email: clean_email, ip: ip })
      { token: token, email: clean_email }
    rescue AuthenticationError
      raise
    rescue StandardError => e
      @events.emit('auth.login_failed', { email: clean_email, ip: ip })
      raise AuthenticationError.new("Authentication failed: #{e}")
    end
  end

  # Verify a token.
  def verify_token(token)
    begin
      user = validate_token(token)
      @users.find_by_email(user.email) if user
    rescue TokenError
      nil
    end
  end

  # Log out.
  def do_logout(token)
    _log('Processing logout')
    session = @sessions.find_active_session(token)
    if session
      @sessions.expire_session(session[:id])
      @events.emit('auth.logout', { session_id: session[:id] })
      return true
    end
    false
  end
end
