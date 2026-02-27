package utils

import (
    "fmt"
    "strings"
    "time"

    "webapp_go/pkg/logger"
)

var log = logger.GetLogger("utils.helpers")

// ValidationResult holds the result of a validation check.
type ValidationResult struct {
    Valid  bool
    Errors []string
}

// ValidateRequest checks that required fields are present in the request.
func ValidateRequest(request map[string]interface{}, requiredFields []string) *ValidationResult {
    log.Info("Validating request with %d required fields", len(requiredFields))
    result := &ValidationResult{Valid: true, Errors: []string{}}
    for _, field := range requiredFields {
        if _, ok := request[field]; !ok {
            result.Valid = false
            result.Errors = append(result.Errors, fmt.Sprintf("missing field: %s", field))
            log.Warn("Missing required field: %s", field)
        }
    }
    if result.Valid {
        log.Info("Request validation passed")
    } else {
        log.Warn("Request validation failed with %d errors", len(result.Errors))
    }
    return result
}

// GenerateRequestID creates a unique request identifier.
func GenerateRequestID() string {
    log.Debug("Generating request ID")
    id := fmt.Sprintf("req_%d", time.Now().UnixNano())
    log.Debug("Generated request ID: %s", id)
    return id
}

// SanitizeInput cleans user input by trimming and escaping special characters.
func SanitizeInput(input string) string {
    log.Debug("Sanitizing input")
    sanitized := strings.TrimSpace(input)
    sanitized = strings.ReplaceAll(sanitized, "<", "&lt;")
    sanitized = strings.ReplaceAll(sanitized, ">", "&gt;")
    sanitized = strings.ReplaceAll(sanitized, "'", "&#39;")
    sanitized = strings.ReplaceAll(sanitized, "\"", "&quot;")
    log.Debug("Input sanitized")
    return sanitized
}

// PaginationResult holds paginated query results.
type PaginationResult struct {
    Items      []interface{}
    Page       int
    PerPage    int
    Total      int
    TotalPages int
}

// Paginate takes a slice and returns a paginated subset.
func Paginate(items []interface{}, page, perPage int) *PaginationResult {
    log.Info("Paginating %d items (page=%d, perPage=%d)", len(items), page, perPage)
    if page < 1 {
        page = 1
    }
    if perPage < 1 {
        perPage = 10
    }
    total := len(items)
    totalPages := (total + perPage - 1) / perPage
    start := (page - 1) * perPage
    end := start + perPage
    if start > total {
        start = total
    }
    if end > total {
        end = total
    }
    result := &PaginationResult{
        Items:      items[start:end],
        Page:       page,
        PerPage:    perPage,
        Total:      total,
        TotalPages: totalPages,
    }
    log.Info("Returning page %d of %d (%d items)", page, totalPages, len(result.Items))
    return result
}
