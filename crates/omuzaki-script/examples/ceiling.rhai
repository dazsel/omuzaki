// omuzaki: grants = ["edit:character"]
// The first line is a ceiling: this script may use at most these scopes, even when you can do more.
// It can only narrow what you hold, never widen it, so a script you share can't reach further than it says.
for name in ["aki", "ren"] {
    ops::face::set_profile(#{ character: name, profile: "egg-oval" });
}
