/* Spike 0.1, harder fixture: the awkward corners of the GCC nested-function
 * extension, not just a function inside a function.
 *
 *   - `auto` forward declaration of a nested function
 *   - `__label__` for non-local goto out of a nested function
 *   - taking the address of a nested function (GCC builds a trampoline)
 *   - a nested function used as a qsort callback
 *
 * Compiles with `gcc -std=gnu11`.
 */
#include <stdio.h>
#include <stdlib.h>

/* Forward-declared nested function: `auto` lets `first` call `second`
 * before `second` is defined. */
int mutual(int n)
{
    auto int second(int);

    int first(int x)
    {
        return x <= 0 ? 0 : second(x - 1);
    }

    int second(int x)
    {
        return x <= 0 ? 0 : first(x - 1) + 1;
    }

    return first(n);
}

/* __label__ declares a local label so a nested function can jump out of
 * the enclosing function entirely. */
int nonlocal_goto(int n)
{
    __label__ bail;
    int seen = 0;

    void check(int v)
    {
        seen++;
        if (v > 3)
            goto bail;
    }

    for (int i = 0; i < n; i++)
        check(i);

    return seen;

bail:
    return -seen;
}

/* Address of a nested function passed to qsort -- the trampoline case. */
int sort_with_nested(int *arr, size_t n, int descending)
{
    int cmp(const void *a, const void *b)
    {
        int x = *(const int *)a;
        int y = *(const int *)b;
        return descending ? (y - x) : (x - y);
    }

    qsort(arr, n, sizeof(int), cmp);
    return arr[0];
}

/* Must still parse cleanly after all of the above. */
static int trailer(int a) { return a * 2; }

int main(void)
{
    int arr[] = { 5, 2, 9, 1 };
    printf("%d %d %d %d\n",
           mutual(6),
           nonlocal_goto(10),
           sort_with_nested(arr, 4, 1),
           trailer(21));
    return 0;
}
