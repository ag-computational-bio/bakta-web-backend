package main

import (
	"log"

	"github.com/ag-computational-bio/bakta-web-backend/endpoints"
)

func main() {
	err := endpoints.RunGrpcJobServer()
	if err != nil {
		log.Fatalln(err.Error())
	}
}
