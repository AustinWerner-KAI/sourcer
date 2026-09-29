/** Shown until the user signs in with their Microsoft 365 account. */
const reasons: Record<string, string> = {
  "not-invited": "This Microsoft account has not been added to Sourcer yet. Ask an admin to add you.",
  expired: "That sign-in took too long or was already used. Please try again.",
  cancelled: "Sign-in was cancelled.",
  microsoft: "Microsoft could not complete the sign-in. Please try again.",
  invalid: "Something went wrong with the sign-in. Please try again.",
};

export function SignIn() {
  const reason = new URLSearchParams(window.location.search).get("signin");
  const message = reason ? (reasons[reason] ?? reasons.invalid) : null;

  return (
    <div className="signin">
      <div className="signin-card">
        <div className="brand">AUSTIN WERNER</div>
        <h1>Sourcer</h1>
        <p className="signin-lead">Resourcing and outreach for the Austin Werner team.</p>
        {message && (
          <p className="signin-error" role="alert">
            {message}
          </p>
        )}
        <a className="btn-primary" href="/api/auth/login">
          Sign in with Microsoft
        </a>
        <p className="signin-note">Use your Austin Werner Microsoft 365 account.</p>
      </div>
    </div>
  );
}
