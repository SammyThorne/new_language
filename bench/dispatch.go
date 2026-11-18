package main

import "fmt"

type Shape interface {
	Area() float64
}

type Circle struct{ radius float64 }
type Square struct{ side float64 }
type Rect struct{ width, height float64 }

func (c *Circle) Area() float64 { return 3.0 * c.radius * c.radius }
func (s *Square) Area() float64 { return s.side * s.side }
func (r *Rect) Area() float64   { return r.width * r.height }

func main() {
	shapes := []Shape{}
	for i := 0; i < 1000; i++ {
		size := float64(i%10 + 1)
		switch i % 3 {
		case 0:
			shapes = append(shapes, &Circle{size})
		case 1:
			shapes = append(shapes, &Square{size})
		default:
			shapes = append(shapes, &Rect{size, 2.0})
		}
	}
	total := 0.0
	for n := 0; n < 10_000; n++ {
		for _, shape := range shapes {
			total += shape.Area()
		}
	}
	fmt.Printf("%.1f\n", total)
}
