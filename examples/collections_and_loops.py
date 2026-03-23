# Basic control flow, loops, comprehensions, and dictionaries.

numbers = [1, 2, 3, 4, 5]
squares = [n * n for n in numbers]

total = 0
for value in squares:
    if value % 2 == 0:
        total = total + value

{
    "numbers": numbers,
    "squares": squares,
    "even_square_total": total,
}
