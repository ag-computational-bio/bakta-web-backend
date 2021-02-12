package endpoints

import (
	"context"
	"log"

	"github.com/ag-computational-bio/bakta-web-api/go/api"
	"github.com/ag-computational-bio/bakta-web-backend/database"
	"github.com/ag-computational-bio/bakta-web-backend/monitor"
	"github.com/ag-computational-bio/bakta-web-backend/scheduler"
)

//BaktaUpdateAPI implements the endpoints of the bakta-web-api
type BaktaUpdateAPI struct {
	api.UnimplementedBaktaStatusUpdateServer
	dbHandler     *database.Handler
	scheduler     *scheduler.SimpleScheduler
	updateMonitor *monitor.SimpleMonitor
}

//UpdateStatus Updates the status of a running job
func (apiHandler *BaktaUpdateAPI) UpdateStatus(ctx context.Context, request *api.UpdateStatusRequest) (*api.Empty, error) {
	go func() {
		status, err := apiHandler.updateMonitor.GetJobStatus(request.GetJobID())
		if err != nil {
			log.Println(err.Error())
			return
		}

		err = apiHandler.dbHandler.UpdateStatus(request.GetJobID(), status.Status, status.ErrorMsg)
		if err != nil {
			log.Println(err.Error())
			return
		}
	}()

	return &api.Empty{}, nil
}
