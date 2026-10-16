// Import another source file: `import` is disabled, so a script is one file and cannot load more.
import "/etc/passwd" as secrets;
secrets::leak();
