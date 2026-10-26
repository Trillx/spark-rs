# spark-rs

Spark-style email for Windows. These terms cover how a copy of spark-rs gets
onto a computer and stays current.

## Language

### Getting spark-rs

**Installer**:
The Windows setup program, with its wizard, that puts spark-rs on a computer for the current user.
_Avoid_: Install wizard, setup.exe, zip

**First-run setup**:
The steps spark-rs walks a new user through in the app before their first mailbox opens.
_Avoid_: Onboarding, install wizard, welcome screen

**Demo mailbox**:
The sample mail spark-rs shows when no real account has been added.
_Avoid_: Demo mode, sample data

### Staying current

**Build**:
An installable copy of spark-rs made from one commit on `main` that changes the app.
_Avoid_: Nightly, snapshot, artifact

**Update**:
Replacing the installed copy with a newer build, when the user chooses to.
_Avoid_: Upgrade, patch

**Commits behind**:
How many app-changing commits the newest build has that the installed copy doesn't.
_Avoid_: Out of date, versions behind

**Release**:
A build whose version number was bumped, which gets a name and release notes.
_Avoid_: Stable, channel
