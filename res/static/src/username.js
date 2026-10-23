// Escape the hyphen for HTML pattern's Unicode-sets regular expression syntax.
export const usernamePattern = String.raw`(?!\.)[A-Za-z0-9_.@+\-]{1,64}`;
const usernameRegex = new RegExp(`^(?:${usernamePattern})$`, "u");

export function validUsername(username) {
    // Require the entire match: JavaScript's $ can also match before a final newline.
    return usernameRegex.exec(username)?.[0] === username;
}
