/* Control fixture: same shape, no nested functions.
 * Both gcc and clang accept this. Used as the baseline to compare
 * tree-sitter output against nested.c. */
#include <stdio.h>

static int shared_helper(int x, int shared)
{
    return x + shared;
}

int outer(int n)
{
    int shared = n * 2;
    int total = 0;

    for (int i = 0; i < n; i++)
        total += shared_helper(i, shared);

    return total;
}

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
