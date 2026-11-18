class Circle:
    __slots__ = ("radius",)

    def __init__(self, radius):
        self.radius = radius

    def area(self) -> float:
        return 3.0 * self.radius * self.radius


class Square:
    __slots__ = ("side",)

    def __init__(self, side):
        self.side = side

    def area(self) -> float:
        return self.side * self.side


class Rect:
    __slots__ = ("width", "height")

    def __init__(self, width, height):
        self.width = width
        self.height = height

    def area(self) -> float:
        return self.width * self.height


shapes = []
for i in range(1000):
    size = float(i % 10 + 1)
    if i % 3 == 0:
        shapes.append(Circle(size))
    elif i % 3 == 1:
        shapes.append(Square(size))
    else:
        shapes.append(Rect(size, 2.0))
total = 0.0
for _ in range(10_000):
    for shape in shapes:
        total += shape.area()
print(f"{total:.1f}")
