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

/* The nine PostgreSQL statistical aggregates, registered as no-ops returning
 * NULL.
 *
 * SQLite has none of them, and the translator emits each verbatim once the
 * caller declares the destination carries it. Without a stub here every corpus
 * row over one would fail with `no such function` on both builds, cancel out
 * of the differential and check nothing. A stub makes SQLite resolve the name
 * and then check what this harness is for: whether the surrounding statement,
 * its window clause, its DISTINCT and its frame, run on the floor version.
 * The values are irrelevant, so the callbacks do nothing.
 */
static void stub_step(sqlite3_context *context, int argc, sqlite3_value **argv) {
    (void)context;
    (void)argc;
    (void)argv;
}

static void stub_final(sqlite3_context *context) { sqlite3_result_null(context); }

static void stub_value(sqlite3_context *context) { sqlite3_result_null(context); }

static int register_statistical_aggregates(sqlite3 *db) {
    static const char *const univariate[] = {"var_pop",  "var_samp", "variance",
                                             "stddev",   "stddev_pop", "stddev_samp"};
    static const char *const bivariate[] = {"covar_pop", "covar_samp", "corr"};
    for (size_t i = 0; i < sizeof(univariate) / sizeof(*univariate); i++) {
        int rc = sqlite3_create_window_function(db, univariate[i], 1, SQLITE_UTF8, NULL, stub_step,
                                                stub_final, stub_value, stub_step, NULL);
        if (rc != SQLITE_OK) return rc;
    }
    for (size_t i = 0; i < sizeof(bivariate) / sizeof(*bivariate); i++) {
        int rc = sqlite3_create_window_function(db, bivariate[i], 2, SQLITE_UTF8, NULL, stub_step,
                                                stub_final, stub_value, stub_step, NULL);
        if (rc != SQLITE_OK) return rc;
    }
    return SQLITE_OK;
}

/* The two UUID generators, registered as stubs returning distinct sixteen-byte
 * blobs.
 *
 * SQLite ships neither: `uuid()` comes from the loadable `uuid.c` extension
 * and a version 7 generator from sqlean. The values need only satisfy the
 * length CHECK the translator puts on a UUID column and differ between calls,
 * so a primary key does not collide for a reason unrelated to the version
 * being checked.
 */
static void stub_uuid(sqlite3_context *context, int argc, sqlite3_value **argv) {
    static unsigned long long next = 1;
    unsigned char value[16] = {0};
    unsigned long long counter = next++;
    (void)argc;
    (void)argv;
    for (int byte = 15; byte >= 8; byte--) {
        value[byte] = (unsigned char)(counter & 0xff);
        counter >>= 8;
    }
    sqlite3_result_blob(context, value, sizeof(value), SQLITE_TRANSIENT);
}

static int register_uuid_generators(sqlite3 *db) {
    static const char *const names[] = {"uuid", "uuid7"};
    for (size_t i = 0; i < sizeof(names) / sizeof(*names); i++) {
        int rc = sqlite3_create_function(db, names[i], 0, SQLITE_UTF8, NULL, stub_uuid, NULL, NULL);
        if (rc != SQLITE_OK) return rc;
    }
    return SQLITE_OK;
}

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
        if (register_statistical_aggregates(db) != SQLITE_OK ||
            register_uuid_generators(db) != SQLITE_OK) {
            printf("FAIL cannot register the stub functions :: %s\n", label);
            failed++;
            sqlite3_close(db);
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
