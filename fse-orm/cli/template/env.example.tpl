# Copy to .env (`fse new` already wrote one with a fresh JWT_SECRET).
ENV=dev # or prod
DATABASE_URL=sqlite:./data/sqlite.db
DOMAIN=localhost # in prod: example.com (no scheme)
PROTOCOL=http # http or https
PORT=8080
JWT_SECRET=replace-me # at least 32 chars: `openssl rand -base64 32`
# Mail (all three or none). Needed for password reset / email verification.
# SMTP_HOST=smtp.example.com:587
# SMTP_USER=info@example.com
# SMTP_PASS=secret
# EMAIL_VERIFICATION_ENABLED=false
# First admin, created at boot if no admin exists yet:
# ADMIN_EMAIL=admin@example.com
# ADMIN_PASSWORD=change-me-now
