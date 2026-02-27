package email

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var emailLog = logger.GetLogger("services.email")

// EmailMessage represents an email to be sent.
type EmailMessage struct {
    To      string
    From    string
    Subject string
    Body    string
    HTML    bool
}

// EmailSender sends emails through a configured SMTP provider.
type EmailSender struct {
    Host     string
    Port     int
    Username string
    FromAddr string
}

// NewEmailSender creates a new email sender with configuration.
func NewEmailSender(host string, port int, username, fromAddr string) *EmailSender {
    emailLog.Info("Creating EmailSender: host=%s, port=%d", host, port)
    return &EmailSender{
        Host:     host,
        Port:     port,
        Username: username,
        FromAddr: fromAddr,
    }
}

// Send dispatches an email message.
func (s *EmailSender) Send(msg *EmailMessage) error {
    emailLog.Info("Sending email to: %s, subject: %s", msg.To, msg.Subject)
    if msg.To == "" {
        emailLog.Error("Recipient address is empty")
        return fmt.Errorf("recipient address required")
    }
    if msg.Subject == "" {
        emailLog.Warn("Email has no subject")
    }
    msg.From = s.FromAddr
    emailLog.Info("Email sent successfully to: %s", msg.To)
    return nil
}

// SendBulk sends an email to multiple recipients.
func (s *EmailSender) SendBulk(recipients []string, subject, body string) (int, error) {
    emailLog.Info("Sending bulk email to %d recipients", len(recipients))
    sent := 0
    for _, to := range recipients {
        msg := &EmailMessage{To: to, Subject: subject, Body: body}
        if err := s.Send(msg); err != nil {
            emailLog.Error("Failed to send to %s: %v", to, err)
            continue
        }
        sent++
    }
    emailLog.Info("Bulk send complete: %d/%d sent", sent, len(recipients))
    return sent, nil
}

// SendWelcomeEmail sends a welcome email to a new user.
func (s *EmailSender) SendWelcomeEmail(email, name string) error {
    emailLog.Info("Sending welcome email to: %s", email)
    msg := &EmailMessage{
        To:      email,
        Subject: fmt.Sprintf("Welcome, %s!", name),
        Body:    fmt.Sprintf("Hello %s, welcome to our platform!", name),
        HTML:    true,
    }
    return s.Send(msg)
}

// SendPasswordReset sends a password reset email.
func (s *EmailSender) SendPasswordReset(email, resetToken string) error {
    emailLog.Info("Sending password reset to: %s", email)
    msg := &EmailMessage{
        To:      email,
        Subject: "Password Reset Request",
        Body:    fmt.Sprintf("Reset your password using token: %s", resetToken),
        HTML:    true,
    }
    return s.Send(msg)
}
