/* Spike 0.1 fixture: GCC nested functions.
 * Compiles with `gcc -std=gnu11`. Rejected by clang/clangd:
 * "function definition is not allowed here" (llvm#9578).
 */
#include <stdio.h>

int outer(int n)
{
    int shared = n * 2;

    /* Nested function closing over `shared`. */
    int helper(int x)
    {
        return x + shared;
    }

    /* Nested function containing a further nested function. */
    int accumulate(int count)
    {
        int total = 0;

        void bump(int v)
        {
            total += v;
        }

        for (int i = 0; i < count; i++)
            bump(helper(i));

        return total;
    }

    return accumulate(n);
}

/* CRITICAL: everything below sits AFTER the nested functions.
 * This is the region clang's parse recovery corrupts -- semantic tokens
 * stop, symbols go wrong. tree-sitter must still see these cleanly. */

struct after_marker {
    int field_a;
    char *field_b;
};

static int after_nested(int a, int b)
{
    return a - b;
}

typedef enum { KIND_ONE, KIND_TWO } after_kind;

int main(void)
{
    struct after_marker m = { .field_a = 1, .field_b = "x" };
    printf("%d %d %s\n", outer(5), after_nested(9, 4), m.field_b);
    return 0;
}
