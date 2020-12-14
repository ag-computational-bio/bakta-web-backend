package main

import (
	"log"

	"github.com/ag-computational-bio/bakta-web-backend/endpoints"
)

func main() {
	// Enable line numbers in logging
	log.SetFlags(log.LstdFlags | log.Lshortfile)

	err := endpoints.RunGrpcJobServer()
	if err != nil {
		log.Fatalln(err.Error())
	}
}
