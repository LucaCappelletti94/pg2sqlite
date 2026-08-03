/* Runs each script in a corpus against the linked SQLite and reports failures.
 *
 * Scripts are separated by a 0x01 byte. Each runs in its own fresh in-memory
 * database. Output is one `FAIL <message> :: <first line of script>` per
 * failure, which `check_sqlite_floor.sh` diffs between two SQLite versions, so
 * a failure present in both (a missing extension, a fragment script) cancels
 * out and only a version-specific failure survives.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "sqlite3.h"

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: runsql <corpus>\n");
        return 2;
    }
    FILE *file = fopen(argv[1], "rb");
    if (!file) {
        perror("open");
        return 2;
    }
    fseek(file, 0, SEEK_END);
    long size = ftell(file);
    fseek(file, 0, SEEK_SET);
    char *buffer = malloc((size_t)size + 1);
    if (!buffer || fread(buffer, 1, (size_t)size, file) != (size_t)size) {
        fprintf(stderr, "read failed\n");
        return 2;
    }
    buffer[size] = 0;
    fclose(file);

    fprintf(stderr, "sqlite %s\n", sqlite3_libversion());
    int total = 0, failed = 0;
    char *save = NULL;
    for (char *script = strtok_r(buffer, "\x01", &save); script;
         script = strtok_r(NULL, "\x01", &save)) {
        while (*script == '\n' || *script == ' ') script++;
        if (!*script) continue;
        total++;

        char label[160];
        size_t n = 0;
        while (script[n] && script[n] != '\n' && n < sizeof(label) - 1) n++;
        memcpy(label, script, n);
        label[n] = 0;

        sqlite3 *db = NULL;
        if (sqlite3_open(":memory:", &db) != SQLITE_OK) {
            printf("FAIL cannot open database :: %s\n", label);
            failed++;
            continue;
        }
        char *error = NULL;
        if (sqlite3_exec(db, script, NULL, NULL, &error) != SQLITE_OK) {
            printf("FAIL %s :: %s\n", error ? error : "unknown", label);
            sqlite3_free(error);
            failed++;
        }
        sqlite3_close(db);
    }
    fprintf(stderr, "total %d, failed %d\n", total, failed);
    return 0;
}
